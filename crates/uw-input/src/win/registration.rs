use super::*;
use crate::registration::{Registration, RegistrationJournal, RegistrationLedger};
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    UI::Input::*,
};
static REGISTER: AtomicUsize = AtomicUsize::new(0);
static RETIRED_TARGET: AtomicUsize = AtomicUsize::new(0);
pub(super) fn retire_window(hwnd: usize) {
    RETIRED_TARGET.store(hwnd, Ordering::Release);
}
static JOURNAL: RegistrationJournal = RegistrationJournal::new();
struct CaptureRegistrations {
    ledger: RegistrationLedger,
    baseline: [u64; 2],
}
static LEDGER: Mutex<Option<CaptureRegistrations>> = Mutex::new(None);
fn convert(r: &RAWINPUTDEVICE) -> Registration {
    Registration { page: r.usUsagePage, usage: r.usUsage, flags: r.dwFlags, target: r.hwndTarget as usize }
}
fn native(r: Registration) -> RAWINPUTDEVICE {
    RAWINPUTDEVICE { usUsagePage: r.page, usUsage: r.usage, dwFlags: r.flags, hwndTarget: r.target as HWND }
}
pub(super) fn targets() -> Vec<hooks::Target> {
    vec![hooks::Target {
        address: RegisterRawInputDevices as *const () as usize,
        detour: register as *const () as usize,
        original: &REGISTER,
    }]
}
unsafe fn call(p: *const RAWINPUTDEVICE, n: u32, size: u32) -> i32 {
    let address = REGISTER.load(Ordering::Acquire);
    // SAFETY: caller has valid descriptors; fallback is used only before installation.
    unsafe {
        if address == 0 {
            RegisterRawInputDevices(p, n, size)
        } else {
            std::mem::transmute::<usize, unsafe extern "system" fn(*const RAWINPUTDEVICE, u32, u32) -> i32>(address)(
                p, n, size,
            )
        }
    }
}
pub(super) fn prepare() -> Option<u64> {
    let mut slot = LEDGER.try_lock().ok()?;
    let token = JOURNAL.begin_owner()?;
    RETIRED_TARGET.store(0, Ordering::Release);
    let baseline = JOURNAL.checkpoint();
    if !JOURNAL.current(token) {
        JOURNAL.cancel_owner();
        return None;
    }
    let success = (|| {
        let mut count = 0;
        // SAFETY: first query only requests descriptor count.
        if unsafe {
            GetRegisteredRawInputDevices(std::ptr::null_mut(), &mut count, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        } == u32::MAX
            || count > 256
        {
            return false;
        }
        let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
        if count != 0 {
            // SAFETY: allocated native descriptor array, API rejects concurrent growth.
            let n = unsafe {
                GetRegisteredRawInputDevices(
                    devices.as_mut_ptr(),
                    &mut count,
                    std::mem::size_of::<RAWINPUTDEVICE>() as u32,
                )
            };
            if n == u32::MAX || n as usize > devices.len() {
                return false;
            }
            devices.truncate(n as usize);
        }
        let ledger = RegistrationLedger::new(&devices.iter().map(convert).collect::<Vec<_>>());
        let changes: Vec<_> = ledger.capture_descriptors().into_iter().map(native).collect();
        // Retain restoration intent even if an overlapping call invalidates this transaction.
        *slot = Some(CaptureRegistrations { ledger, baseline });
        changes.is_empty()
            // SAFETY: initialized descriptors and native element size.
            || unsafe { call(changes.as_ptr(), changes.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } != 0
    })();
    if success {
        Some(token)
    } else {
        JOURNAL.cancel_owner();
        None
    }
}
pub(super) fn activate(token: u64, success: bool) -> bool {
    if success {
        JOURNAL.commit_capture(token)
    } else {
        JOURNAL.cancel_owner();
        false
    }
}
#[cfg(test)]
fn enter() -> bool {
    prepare().is_some_and(|token| activate(token, true))
}
pub(super) fn restore() -> bool {
    let Ok(mut slot) = LEDGER.try_lock() else { return false };
    let Some(ledger) = slot.as_mut() else { return true };
    let Some(token) = JOURNAL.begin_owner() else { return false };
    ledger.ledger.retire_target(RETIRED_TARGET.load(Ordering::Acquire));
    JOURNAL.merge_since(&mut ledger.ledger, ledger.baseline);
    for entry in ledger.ledger.restore_descriptors() {
        // SAFETY: discard targets destroyed since their latest successful registration.
        if entry.target != 0
            && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(entry.target as HWND) } == 0
        {
            ledger.ledger.retire_target(entry.target);
        }
    }
    let changes: Vec<_> = ledger.ledger.restore_descriptors().into_iter().map(native).collect();
    if !changes.is_empty()
        // SAFETY: latest successful game descriptors, including explicit removals.
        && unsafe { call(changes.as_ptr(), changes.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } == 0
    {
        JOURNAL.cancel_owner();
        return false;
    }
    if !JOURNAL.commit_owner(token) {
        return false;
    }
    *slot = None;
    true
}
unsafe extern "system" fn register(ptr: *const RAWINPUTDEVICE, count: u32, size: u32) -> i32 {
    boundary(0, || {
        if bypassed() || size as usize != std::mem::size_of::<RAWINPUTDEVICE>() || ptr.is_null() {
            // SAFETY: unchanged original caller arguments; original API handles errors.
            return unsafe { call(ptr, count, size) };
        }
        let transaction = JOURNAL.begin_call();
        if transaction.interfered()
            && let Some(s) = SHARED.get()
        {
            s.revoke(InputReason::Unavailable);
        }
        let active = transaction.captured();
        let transform = active && count <= 256;
        if active
            && !transform
            && let Some(s) = SHARED.get()
        {
            s.revoke(InputReason::Unavailable);
        }
        // SAFETY: original caller's descriptor array is required to be readable for count entries.
        let input = unsafe { std::slice::from_raw_parts(ptr, count as usize) };
        let mut transformed = [RAWINPUTDEVICE::default(); 256];
        if transform {
            for (i, r) in input.iter().enumerate() {
                transformed[i] = native(convert(r).captured());
            }
        }
        // SAFETY: either initialized transformed prefix or unchanged readable caller descriptors.
        let result = unsafe { call(if transform { transformed.as_ptr() } else { ptr }, count, size) };
        // SAFETY: preserves original registration result's error state.
        let error = unsafe { GetLastError() };
        if result != 0 {
            for r in input {
                transaction.success(convert(r));
            }
        }
        drop(transaction);
        // SAFETY: restore caller-visible error after bookkeeping.
        unsafe { SetLastError(error) };
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, OnceLock};
    static OWNER_THREAD: AtomicU32 = AtomicU32::new(0);
    static PAUSE: AtomicBool = AtomicBool::new(false);
    static AFTER: AtomicBool = AtomicBool::new(false);
    static BARRIERS: OnceLock<(Arc<Barrier>, Arc<Barrier>)> = OnceLock::new();
    unsafe extern "system" fn controlled_call(p: *const RAWINPUTDEVICE, n: u32, size: u32) -> i32 {
        // SAFETY: thread id query has no arguments.
        let pause = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
            == OWNER_THREAD.load(Ordering::Acquire)
            && PAUSE.swap(false, Ordering::AcqRel);
        let barrier = || {
            let (entered, resume) = BARRIERS.get().unwrap();
            entered.wait();
            resume.wait();
        };
        if pause && !AFTER.load(Ordering::Acquire) {
            barrier();
        }
        // SAFETY: this test trampoline forwards the untouched native caller array.
        let result = unsafe { RegisterRawInputDevices(p, n, size) };
        // SAFETY: capture native result before test synchronization changes thread error state.
        let error = unsafe { GetLastError() };
        if pause && AFTER.load(Ordering::Acquire) {
            barrier();
        }
        // SAFETY: preserve native API last-error contract through the test trampoline.
        unsafe {
            SetLastError(error);
        }
        result
    }
    fn game(flags: u32) {
        let r = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: flags, hwndTarget: std::ptr::null_mut() };
        // SAFETY: initialized native descriptor; hook calls the native API exactly once.
        assert_ne!(unsafe { register(&r, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }, 0);
    }
    fn flags() -> Option<u32> {
        let mut n = 16;
        let mut devices = [RAWINPUTDEVICE::default(); 16];
        // SAFETY: initialized output array with matching capacity and native size.
        let count = unsafe {
            GetRegisteredRawInputDevices(devices.as_mut_ptr(), &mut n, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        };
        assert_ne!(count, u32::MAX);
        devices[..count as usize].iter().find(|r| r.usUsagePage == 1 && r.usUsage == 6).map(|r| r.dwFlags)
    }
    fn overlap(operation: fn() -> bool, latest: u32, after: bool) {
        let (entered, resume) = BARRIERS.get().unwrap();
        AFTER.store(after, Ordering::Release);
        PAUSE.store(true, Ordering::Release);
        let owner = std::thread::spawn(move || {
            // SAFETY: no-argument query identifies this test's owner transaction.
            OWNER_THREAD
                .store(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }, Ordering::Release);
            operation()
        });
        entered.wait();
        game(latest);
        resume.wait();
        assert!(!owner.join().unwrap(), "overlapping registration incorrectly committed");
    }
    #[test]
    fn native_registration_overlap_reconciles_success_removal_and_errors() {
        BARRIERS.set((Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)))).unwrap();
        REGISTER.store(controlled_call as *const () as usize, Ordering::Release);
        game(RIDEV_NOLEGACY);
        overlap(enter, RIDEV_NOLEGACY | RIDEV_NOHOTKEYS, false);
        assert!(restore());
        assert_eq!(flags(), Some(RIDEV_NOLEGACY | RIDEV_NOHOTKEYS));
        assert!(enter());
        overlap(restore, RIDEV_NOLEGACY, true);
        overlap(restore, RIDEV_REMOVE, false);
        assert!(restore());
        assert_eq!(flags(), None);
        // Invalid size must preserve the original error and leave successful intent unchanged.
        let r = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, ..Default::default() };
        // SAFETY: readable descriptor, deliberate invalid scalar size is handled by Win32.
        let expected = unsafe { RegisterRawInputDevices(&r, 1, 1) };
        // SAFETY: immediate thread-local error query.
        let expected_error = unsafe { GetLastError() };
        // SAFETY: same valid buffer and invalid scalar size through the adapter.
        assert_eq!(unsafe { register(&r, 1, 1) }, expected);
        // SAFETY: immediate thread-local error query.
        assert_eq!(unsafe { GetLastError() }, expected_error);
        assert_eq!(flags(), None);
    }
}
