//! System-method detours with temporary owned SDK references; UI-only acquisition and bounded reference cleanup.
use super::{
    abi::{Raw, directinput as abi},
    hooks, *,
};
use crate::{
    directinput::{DeviceKind, DeviceRegistry, DeviceToken},
    native::{AdapterCoverage, DiscoveryReport, DiscoveryStatus, NativeApi},
};
use windows_sys::core::GUID;
static DEVICES: DeviceRegistry = DeviceRegistry::new();
static HEALTHY: AtomicBool = AtomicBool::new(true);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static DEFERRED_PENDING: AtomicBool = AtomicBool::new(false);
static LIFETIME: AtomicBool = AtomicBool::new(false);
static DEFERRED: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
static INSTALL: Mutex<()> = Mutex::new(());
struct Method {
    address: AtomicUsize,
    signature: AtomicUsize,
    original: AtomicUsize,
}
static METHODS: [Method; 256] =
    [const { Method { address: AtomicUsize::new(0), signature: AtomicUsize::new(0), original: AtomicUsize::new(0) } };
        256];
const NOT_ACQUIRED: i32 = 0x8007_000cu32 as i32;
const NOT_BUFFERED: i32 = 0x8004_0207u32 as i32;
const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
thread_local! { static RELEASING: Cell<(usize, usize)> = const { Cell::new((0, 0)) }; }
thread_local! { static INTERNAL: Cell<(usize, usize)> = const { Cell::new((0, 0)) }; }
fn internal_call<T>(pair: (usize, usize), operation: impl FnOnce() -> T) -> T {
    struct Guard((usize, usize));
    impl Drop for Guard {
        fn drop(&mut self) {
            INTERNAL.with(|r| r.set(self.0));
        }
    }
    let _guard = Guard(INTERNAL.with(|r| r.replace(pair)));
    operation()
}
fn internal(this: Raw) -> bool {
    INTERNAL.with(|r| {
        let (identity, source) = r.get();
        this as usize == identity || this as usize == source
    })
}
pub(super) fn game_callback<T>(operation: impl FnOnce() -> T) -> T {
    internal_call((0, 0), operation)
}
fn releasing(this: Raw) -> bool {
    RELEASING.with(|r| {
        let (identity, source) = r.get();
        this as usize == identity || this as usize == source
    })
}
fn final_release<T>(identity: usize, this: Raw, operation: impl FnOnce() -> T) -> T {
    struct Guard((usize, usize));
    impl Drop for Guard {
        fn drop(&mut self) {
            RELEASING.with(|r| r.set(self.0));
        }
    }
    let _guard = Guard(RELEASING.with(|r| r.replace((identity, this as usize))));
    operation()
}
pub(super) fn healthy() -> bool {
    HEALTHY.load(Ordering::Acquire)
}
/// Release a reference delayed only while the UI obtains its own temporary reference.
/// Contended releases transfer a real caller reference; no unowned address is deferred.
pub(super) fn drain_deferred() {
    if !DEFERRED_PENDING.swap(false, Ordering::AcqRel) {
        return;
    }
    for (index, slot) in DEFERRED.iter().enumerate() {
        let count = slot.swap(0, Ordering::AcqRel);
        if count == 0 {
            continue;
        }
        let Some(token) = DEVICES.token_at(index) else {
            fail();
            continue;
        };
        // Each count owns an actual canonical IUnknown reference, so its identity cannot be recycled.
        // Bound each drain, retaining any excess owned references for the next worker/UI service.
        if count > 64 {
            slot.fetch_add(count - 64, Ordering::AcqRel);
            DEFERRED_PENDING.store(true, Ordering::Release);
        }
        for _ in 0..count.min(64) {
            // SAFETY: this count transfers one real canonical IUnknown reference to the consumer.
            unsafe {
                release(token.identity() as Raw);
            }
        }
    }
}
pub(super) fn synchronize(force_owned: bool) {
    if !INSTALLED.load(Ordering::Acquire) {
        return;
    }
    let Some(s) = SHARED.get() else {
        return;
    };
    // SAFETY: acquisition/cursor side effects stay on the selected GUI owner.
    if unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() } != s.thread() {
        return;
    }
    drain_deferred();
    for index in 0..256 {
        let Some(token) = DEVICES.token_at(index) else {
            continue;
        };
        if token.kind() == DeviceKind::Other {
            continue;
        }
        let (owned, held) = eligibility(token);
        let restore = DEVICES.intent(token).is_some_and(|intent| intent.may_restore(held));
        if !(owned && force_owned || !owned && restore) {
            continue;
        }
        if LIFETIME.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            continue;
        }
        let mut output = std::ptr::null_mut();
        let iid = GUID::from_u128(0x54d41081_dc15_4833_a41b_748f73a38179);
        let valid = DEVICES.intent(token).is_some();
        let hr = if valid {
            let pointer = token.identity() as Raw;
            // SAFETY: registered live IUnknown prefix, protected against final native release by the gate.
            let query = unsafe {
                std::mem::transmute::<usize, super::abi::QueryInterface>(original_address(address(pointer, 0)))
            };
            // SAFETY: final Release cannot free this registered identity while LIFETIME is held.
            // QI is the only native call under this gate, yielding an explicitly owned SDK interface.
            unsafe { query(pointer, &iid, &mut output) }
        } else {
            -1
        };
        LIFETIME.store(false, Ordering::Release);
        if hr >= 0 && !output.is_null() {
            // SAFETY: successful QI supplies a temporary Device8W reference, independent of game release.
            unsafe {
                isolate_or_restore(output, token);
                release(output);
            }
        }
    }
    drain_deferred();
}
fn fail() {
    HEALTHY.store(false, Ordering::Release);
    native::request_coverage();
    if let Some(s) = SHARED.get() {
        s.revoke(InputReason::Unavailable);
    }
}
pub fn install(report: &DiscoveryReport) -> Result<AdapterCoverage, InstallError> {
    let result = (|| {
        let _install = INSTALL.lock().map_err(|_| InstallError("DirectInput installation poisoned".into()))?;
        let mut targets = Vec::new();
        for target in report.targets.iter().filter(|t| t.api == NativeApi::DirectInput) {
            if target.address == 0 {
                return Err(InstallError("null DirectInput target".into()));
            }
            if let Some(old) = METHODS.iter().find(|m| m.address.load(Ordering::Acquire) == target.address) {
                if old.signature.load(Ordering::Acquire) != target.signature.0 as usize {
                    return Err(InstallError("incompatible DirectInput target alias".into()));
                }
                continue;
            }
            let slot = METHODS
                .iter()
                .find(|m| m.address.load(Ordering::Acquire) == 0)
                .ok_or_else(|| InstallError("DirectInput method capacity".into()))?;
            let detour = match target.signature.0 {
                1 => query as *const () as usize,
                2 => add_ref as *const () as usize,
                3 => release as *const () as usize,
                10 => factory as *const () as usize,
                11 => create_device as *const () as usize,
                20 => acquire as *const () as usize,
                21 => unacquire as *const () as usize,
                22 => state as *const () as usize,
                23 => data as *const () as usize,
                24 => format as *const () as usize,
                25 => cooperative as *const () as usize,
                _ => return Err(InstallError("unknown DirectInput method signature".into())),
            };
            slot.signature.store(target.signature.0 as usize, Ordering::Release);
            slot.address.store(target.address, Ordering::Release);
            targets.push(hooks::Target { address: target.address, detour, original: &slot.original });
        }
        if !targets.is_empty() {
            let pairs = targets.iter().map(|t| (t.address, t.detour)).collect::<Vec<_>>();
            crate::hooks::HookSet::install(&mut hooks::Backend(&targets), &pairs)
                .map_err(|e| InstallError(e.message))?;
        }
        if !healthy() {
            return Err(InstallError("uncovered DirectInput interface or registry contention".into()));
        }
        INSTALLED.store(true, Ordering::Release);
        lowlevel::request();
        Ok(AdapterCoverage { status: DiscoveryStatus::Supported, abi_mask: report.directinput.abi_mask, detail: 0 })
    })();
    if result.is_err() {
        fail();
    }
    result
}
unsafe fn address(this: Raw, index: usize) -> usize {
    // SAFETY: each caller has the SDK's live interface reference and its IID-specific slot bound.
    unsafe { *(*(this.cast::<*const usize>())).add(index) }
}
fn original_address(address: usize) -> usize {
    METHODS
        .iter()
        .find(|m| m.address.load(Ordering::Acquire) == address)
        .map(|m| m.original.load(Ordering::Acquire))
        .filter(|p| *p != 0)
        .unwrap_or(address)
}
macro_rules! call {
    ($this:expr, $index:expr, $ty:ty) => {{
        // SAFETY: caller selects this exact SDK signature for the live interface's method slot.
        unsafe { std::mem::transmute::<usize, $ty>(original_address(address($this, $index))) }
    }};
}
unsafe fn identity(this: Raw) -> Option<usize> {
    let query = call!(this, 0, super::abi::QueryInterface);
    let mut output = std::ptr::null_mut();
    // SAFETY: all intercepted objects have IUnknown; the temporary returned reference is balanced below.
    if unsafe { query(this, &UNKNOWN, &mut output) } < 0 || output.is_null() {
        return None;
    }
    let release = call!(output, 2, super::abi::Reference);
    let key = output as usize;
    // SAFETY: release only this function's temporary QI reference through the original method.
    unsafe {
        release(output);
    }
    Some(key)
}
unsafe fn token(this: Raw, created: bool) -> Option<DeviceToken> {
    // SAFETY: this function is called only on real IDirectInputDevice8 A/W references.
    let key = unsafe { identity(this) }?;
    if !created && let Some(token) = DEVICES.find(key) {
        return Some(token);
    }
    let capabilities = call!(this, 3, unsafe extern "system" fn(Raw, *mut u32) -> i32);
    let mut caps = [0u32; 11];
    caps[0] = std::mem::size_of_val(&caps) as u32;
    // SAFETY: initialized DIDEVCAPS has the same layout for A/W, independent of custom data formats.
    if unsafe { capabilities(this, caps.as_mut_ptr()) } < 0 {
        fail();
        return None;
    }
    if matches!(caps[2] & 255, 2 | 3) {
        fail();
        return None;
    }
    let kind = DeviceKind::from_native_type(caps[2]);
    let result = if created { DEVICES.created(key, kind) } else { DEVICES.observe(key, kind) };
    match result {
        Ok(token) => Some(token),
        Err(_) => {
            fail();
            None
        }
    }
}
fn eligibility(token: DeviceToken) -> (bool, bool) {
    if token.kind() == DeviceKind::Other {
        return (false, false);
    }
    let Some(s) = SHARED.get() else {
        return (false, false);
    };
    let mouse = token.kind() == DeviceKind::Mouse;
    let owned = s.capture_domain(mouse);
    let activation_pending = matches!(s.release.load(Ordering::Acquire),
        value if value == InputReason::FocusLost as usize + 1 || value == InputReason::WindowDestroyed as usize + 1);
    let held = activation_pending
        || if mouse {
            [1, 2, 4, 5, 6].into_iter().any(|vk| s.suppressed_key(vk))
        } else {
            (1..256).any(|vk| !matches!(vk, 1 | 2 | 4 | 5 | 6) && s.suppressed_key(vk))
        };
    (owned, held)
}
unsafe fn isolate_or_restore(this: Raw, token: DeviceToken) -> bool {
    if token.kind() == DeviceKind::Other {
        return false;
    }
    let (owned, held) = eligibility(token);
    let Some(intent) = DEVICES.intent(token) else {
        fail();
        return owned;
    };
    let unacquire = call!(this, 8, abi::Acquire);
    let pair = (token.identity(), this as usize);
    if owned || (intent.is_isolated() && held) {
        // SAFETY: caller retains the device; suppress only native A/W intent forwarding.
        let hr = internal_call(pair, || unsafe { unacquire(this) });
        if hr < 0 || DEVICES.apply(token, |intent| intent.isolated(hr)).is_err() {
            fail();
        }
        return true;
    }
    if intent.may_restore(held) {
        let acquire = call!(this, 7, abi::Acquire);
        // SAFETY: original cooperative/format validation remains in force on the caller's live reference.
        let hr = internal_call(pair, || unsafe { acquire(this) });
        if hr >= 0 {
            let read = call!(this, 10, abi::GetDeviceData);
            let mut count = u32::MAX;
            // SAFETY: documented NULL/INFINITE x64 SDK flush; custom game layouts are never interpreted.
            let flushed = unsafe { read(this, 24, std::ptr::null_mut(), &mut count, 0) };
            let wanted = DEVICES.intent(token).is_some_and(|intent| intent.restore_requested());
            if !wanted || (flushed < 0 && flushed != NOT_BUFFERED) {
                // SAFETY: an intervening game Unacquire or failed discard must win over restoration.
                internal_call(pair, || unsafe { unacquire(this) });
                if wanted {
                    fail();
                }
            } else if DEVICES.apply(token, |intent| intent.restored(hr)).is_err() {
                fail();
            }
        }
    }
    DEVICES.intent(token).is_none_or(|intent| intent.is_isolated())
}
unsafe extern "system" fn factory(instance: Raw, version: u32, iid: *const GUID, output: *mut Raw, outer: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let address = METHODS
            .iter()
            .find(|m| m.signature.load(Ordering::Acquire) == 10)
            .map(|m| m.original.load(Ordering::Acquire))
            .unwrap_or(0);
        // SAFETY: installed real DirectInput8Create trampoline, with unchanged SDK arguments.
        unsafe { std::mem::transmute::<usize, abi::Create>(address)(instance, version, iid, output, outer) }
    })
}
unsafe extern "system" fn create_device(this: Raw, guid: *const GUID, output: *mut Raw, outer: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 3, abi::CreateDevice);
        // SAFETY: original validates and owns all factory arguments and writes output on success.
        let hr = unsafe { original(this, guid, output, outer) };
        if hr >= 0 && !output.is_null() {
            // SAFETY: a successful original CreateDevice returned one live device reference.
            unsafe {
                if !(*output).is_null() {
                    token(*output, true);
                }
            }
        }
        hr
    })
}
unsafe extern "system" fn query(this: Raw, iid: *const GUID, output: *mut Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 0, super::abi::QueryInterface);
        // SAFETY: preserve original QI identity, output and reference count contracts.
        let hr = unsafe { original(this, iid, output) };
        if hr >= 0 && !output.is_null() {
            // SAFETY: QI success provides a live IUnknown prefix; validate coverage without retaining it.
            unsafe {
                if !(*output).is_null() {
                    for index in 0..3 {
                        let a = address(*output, index);
                        if !METHODS.iter().any(|m| m.address.load(Ordering::Acquire) == a) {
                            fail();
                        }
                    }
                }
            }
        }
        hr
    })
}
unsafe extern "system" fn add_ref(this: Raw) -> u32 {
    boundary(0, || {
        let original = call!(this, 1, super::abi::Reference);
        // SAFETY: caller's live reference and original ABI; the adapter owns no hidden COM reference.
        unsafe { original(this) }
    })
}
unsafe extern "system" fn release(this: Raw) -> u32 {
    boundary(0, || {
        let original = call!(this, 2, super::abi::Reference);
        if releasing(this) {
            // SAFETY: native A/W forwarding during this object's original Release must not resurrect it.
            return unsafe { original(this) };
        }
        let query = call!(this, 0, super::abi::QueryInterface);
        let mut unknown = std::ptr::null_mut();
        // SAFETY: retain a canonical reference while the caller's original reference is still live.
        if unsafe { query(this, &UNKNOWN, &mut unknown) } < 0 || unknown.is_null() {
            // SAFETY: preserve the caller's original release if canonical identity is unavailable.
            return unsafe { original(this) };
        }
        let key = unknown as usize;
        let drop_unknown = call!(unknown, 2, super::abi::Reference);
        let Some(token) = DEVICES.find(key) else {
            // SAFETY: balance the owned QI reference; the caller's reference still keeps storage live.
            final_release(key, unknown, || unsafe { drop_unknown(unknown) });
            // SAFETY: unregistered objects are never borrowed by the UI.
            return final_release(key, this, || unsafe { original(this) });
        };
        if LIFETIME.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            // SAFETY: the canonical reference prevents final destruction during this original release.
            final_release(key, this, || unsafe { original(this) });
            // Transfer the actual canonical reference, counted in this live identity's registry slot.
            // A u64 count exceeds the native u32 COM reference-count range; no fixed queue can fill.
            DEFERRED[token.slot()].fetch_add(1, Ordering::AcqRel);
            DEFERRED_PENDING.store(true, Ordering::Release);
            return 1;
        }
        struct Lifetime;
        impl Drop for Lifetime {
            fn drop(&mut self) {
                LIFETIME.store(false, Ordering::Release);
            }
        }
        let _lifetime = Lifetime;
        // SAFETY: all final decrements share the gate; this hook's canonical reference keeps storage live.
        let remaining = final_release(key, this, || unsafe { original(this) });
        if remaining == 1 {
            DEVICES.retire(token);
        }
        // SAFETY: retire before final destruction, then balance exactly this hook's canonical QI reference.
        final_release(key, unknown, || unsafe { drop_unknown(unknown) })
    })
}
unsafe extern "system" fn acquire(this: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 7, abi::Acquire);
        if internal(this) {
            // SAFETY: native A/W forwarding, already accounted for by its outer call.
            return unsafe { original(this) };
        }
        // SAFETY: Acquire supplies a live SDK device reference.
        let token = unsafe { token(this, false) };
        let pair = (token.map_or(this as usize, DeviceToken::identity), this as usize);
        // SAFETY: native Acquire validates the original cooperative/data-format intent.
        let hr = internal_call(pair, || unsafe { original(this) });
        if let Some(token) = token {
            if DEVICES.apply(token, |intent| intent.acquired(hr)).is_err() {
                fail();
            }
            // SAFETY: the current caller retains the reference throughout isolation/restoration.
            if unsafe { isolate_or_restore(this, token) } && hr >= 0 {
                return NOT_ACQUIRED;
            }
        }
        hr
    })
}
unsafe extern "system" fn unacquire(this: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 8, abi::Acquire);
        if releasing(this) || internal(this) {
            // SAFETY: native cleanup/forwarding must not query or revive a dying object.
            return unsafe { original(this) };
        }
        // SAFETY: Unacquire supplies a live SDK device reference.
        let token = unsafe { token(this, false) };
        let isolated = token.and_then(|token| DEVICES.intent(token)).is_some_and(|intent| intent.is_isolated());
        let pair = (token.map_or(this as usize, DeviceToken::identity), this as usize);
        // SAFETY: forward native Unacquire, preserving its HRESULT.
        let hr = internal_call(pair, || unsafe { original(this) });
        if let Some(token) = token {
            if DEVICES.apply(token, |intent| intent.unacquired(hr)).is_err() {
                fail();
            }
            if isolated && hr >= 0 {
                // SAFETY: publish game intent before cancelling an overlapping temporary restoration.
                let forced = internal_call(pair, || unsafe { original(this) });
                if DEVICES.apply(token, |intent| intent.isolated(forced)).is_err() {
                    fail();
                }
            }
        }
        hr
    })
}
unsafe extern "system" fn state(this: Raw, bytes: u32, output: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        // SAFETY: state reads identify a real live device and isolate before the original read.
        let isolated = unsafe { token(this, false).is_some_and(|t| isolate_or_restore(this, t)) };
        let original = call!(this, 9, abi::GetDeviceState);
        // SAFETY: original validates and writes the unchanged caller buffer/size.
        let hr = unsafe { original(this, bytes, output) };
        if isolated && hr >= 0 { NOT_ACQUIRED } else { hr }
    })
}
unsafe extern "system" fn data(this: Raw, bytes: u32, output: Raw, count: *mut u32, flags: u32) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        // SAFETY: buffered reads identify a real live device, regardless of PEEK, flush or custom format.
        let isolated = unsafe { token(this, false).is_some_and(|t| isolate_or_restore(this, t)) };
        let original = call!(this, 10, abi::GetDeviceData);
        // SAFETY: native validates original size/count/flags and output buffer contracts.
        let hr = unsafe { original(this, bytes, output, count, flags) };
        if isolated && hr >= 0 { NOT_ACQUIRED } else { hr }
    })
}
unsafe extern "system" fn format(this: Raw, format: Raw) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 11, unsafe extern "system" fn(Raw, Raw) -> i32);
        // SAFETY: unchanged SDK format pointer; only the native API interprets or validates its bounds.
        let hr = unsafe { original(this, format) };
        if hr >= 0 {
            // SAFETY: successful native format validation establishes this live device's interface.
            unsafe {
                token(this, false);
            }
        }
        hr
    })
}
unsafe extern "system" fn cooperative(this: Raw, hwnd: HWND, flags: u32) -> i32 {
    boundary(0x8000_ffffu32 as i32, || {
        let original = call!(this, 13, unsafe extern "system" fn(Raw, HWND, u32) -> i32);
        // SAFETY: unchanged cooperative-level arguments, and original native return value.
        unsafe { original(this, hwnd, flags) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    static SERIAL: Mutex<()> = Mutex::new(());
    static PAUSED: AtomicBool = AtomicBool::new(false);
    static SECOND_DONE: AtomicBool = AtomicBool::new(false);
    #[repr(C)]
    struct Object {
        table: *const usize,
        references: AtomicU32,
    }
    unsafe extern "system" fn query(this: Raw, _: *const GUID, output: *mut Raw) -> i32 {
        // SAFETY: test-owned object and writable QI output supplied by the adapter.
        unsafe {
            add(this);
            *output = this;
        }
        0
    }
    unsafe extern "system" fn add(this: Raw) -> u32 {
        // SAFETY: fixture storage remains live for this complete test.
        unsafe { &*this.cast::<Object>() }.references.fetch_add(1, Ordering::AcqRel) + 1
    }
    unsafe extern "system" fn drop_reference(this: Raw) -> u32 {
        // SAFETY: fixture storage outlives all calls, including simulated final Release.
        let count = unsafe { &*this.cast::<Object>() }.references.fetch_sub(1, Ordering::AcqRel) - 1;
        if releasing(this) && !PAUSED.swap(true, Ordering::AcqRel) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !SECOND_DONE.load(Ordering::Acquire) {
                assert!(std::time::Instant::now() < deadline, "second Release did not finish");
                std::thread::yield_now();
            }
        }
        count
    }
    #[test]
    fn contended_releases_are_counted_without_a_fixed_queue_and_drained_in_batches() {
        let _serial = SERIAL.lock().unwrap();
        PAUSED.store(true, Ordering::Release);
        SECOND_DONE.store(true, Ordering::Release);
        let table = [query as *const () as usize, add as *const () as usize, drop_reference as *const () as usize];
        let mut object = Object { table: table.as_ptr(), references: AtomicU32::new(1025) };
        let pointer = (&mut object as *mut Object) as usize;
        DEVICES.observe(pointer, DeviceKind::Keyboard).unwrap();
        LIFETIME.store(true, Ordering::Release);
        for _ in 0..1025 {
            // SAFETY: each call transfers one of the fixture's initial real owned references.
            unsafe {
                release(pointer as Raw);
            }
        }
        LIFETIME.store(false, Ordering::Release);
        drain_deferred();
        assert_eq!(object.references.load(Ordering::Acquire), 1025 - 64);
        assert!(DEVICES.find(pointer).is_some());
        for _ in 0..16 {
            drain_deferred();
        }
        assert_eq!(object.references.load(Ordering::Acquire), 0);
        assert!(DEVICES.find(pointer).is_none());
    }
    #[test]
    fn installation_failure_cannot_be_reported_supported_on_retry() {
        let report = DiscoveryReport {
            targets: vec![crate::native::DiscoveryTarget {
                api: NativeApi::DirectInput,
                abi: 8,
                method: "invalid test export",
                address: 0,
                signature: crate::native::SignatureId(1),
            }],
            ..DiscoveryReport::default()
        };
        assert!(install(&report).is_err());
        assert!(install(&DiscoveryReport::default()).is_err());
    }
    #[test]
    fn concurrent_last_releases_retire_before_the_next_ui_borrow() {
        let _serial = SERIAL.lock().unwrap();
        PAUSED.store(false, Ordering::Release);
        SECOND_DONE.store(false, Ordering::Release);
        let table = [query as *const () as usize, add as *const () as usize, drop_reference as *const () as usize];
        let mut object = Object { table: table.as_ptr(), references: AtomicU32::new(2) };
        let pointer = (&mut object as *mut Object) as usize;
        DEVICES.observe(pointer, DeviceKind::Keyboard).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                // SAFETY: this thread receives one initial owned reference.
                unsafe {
                    release(pointer as Raw);
                }
            });
            while !PAUSED.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            // SAFETY: this thread receives the second initial owned reference.
            unsafe {
                release(pointer as Raw);
            }
            SECOND_DONE.store(true, Ordering::Release);
        });
        drain_deferred();
        assert_eq!(object.references.load(Ordering::Acquire), 0);
        assert!(DEVICES.find(pointer).is_none(), "freed native identity remained available for UI QI");
    }
}
