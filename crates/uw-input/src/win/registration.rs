use super::*;
use crate::registration::{Registration, RegistrationLedger};
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    UI::Input::*,
};
static REGISTER: AtomicUsize = AtomicUsize::new(0);
static LEDGER: Mutex<Option<RegistrationLedger>> = Mutex::new(None);
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
pub(super) fn enter() -> bool {
    let mut count = 0;
    // SAFETY: first query only requests descriptor count.
    if unsafe {
        GetRegisteredRawInputDevices(std::ptr::null_mut(), &mut count, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
    } == u32::MAX
    {
        return false;
    }
    if count > 256 {
        return false;
    }
    let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
    if count != 0 {
        // SAFETY: capacity matches queried count, original API handles concurrent count changes by failing.
        let n = unsafe {
            GetRegisteredRawInputDevices(devices.as_mut_ptr(), &mut count, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        };
        if n == u32::MAX || n as usize > devices.len() {
            return false;
        }
        devices.truncate(n as usize);
    }
    let Ok(mut slot) = LEDGER.try_lock() else { return false };
    let ledger = RegistrationLedger::new(&devices.iter().map(convert).collect::<Vec<_>>());
    let changes: Vec<_> = ledger.capture_descriptors().into_iter().map(native).collect();
    // SAFETY: descriptors are owned and retain original targets, changing only capture flags.
    if !changes.is_empty()
        // SAFETY: owned initialized descriptor array and native element size.
        && unsafe { call(changes.as_ptr(), changes.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } == 0
    {
        return false;
    }
    *slot = Some(ledger);
    true
}
pub(super) fn restore() -> bool {
    let Ok(mut slot) = LEDGER.try_lock() else { return false };
    let Some(ledger) = slot.as_ref() else { return true };
    let changes: Vec<_> = ledger.restore_descriptors().into_iter().map(native).collect();
    // SAFETY: validated game-intended descriptors are owned and alive for the call.
    if !changes.is_empty()
        // SAFETY: owned initialized descriptor array and native element size.
        && unsafe { call(changes.as_ptr(), changes.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } == 0
    {
        return false;
    }
    *slot = None;
    true
}
unsafe extern "system" fn register(ptr: *const RAWINPUTDEVICE, count: u32, size: u32) -> i32 {
    boundary(0, || {
        let active = SHARED.get().is_some_and(|s| s.capture()) && !bypassed();
        if bypassed() || size as usize != std::mem::size_of::<RAWINPUTDEVICE>() || ptr.is_null() {
            // SAFETY: unchanged original caller arguments; original API handles errors.
            return unsafe { call(ptr, count, size) };
        }
        let Ok(mut slot) = LEDGER.try_lock() else {
            if let Some(s) = SHARED.get() {
                s.revoke(InputReason::Unavailable)
            }
            // SAFETY: forwards caller arguments after capture permission is revoked.
            return unsafe { call(ptr, count, size) };
        };
        if !active && slot.is_none() {
            // SAFETY: unchanged caller arguments outside a capture/restoration transaction.
            return unsafe { call(ptr, count, size) };
        }
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
        if result != 0
            && let Some(ledger) = slot.as_mut()
        {
            for r in input {
                ledger.record_success(&[convert(r)])
            }
        }
        // SAFETY: restore caller-visible error after bookkeeping.
        unsafe { SetLastError(error) };
        result
    })
}
