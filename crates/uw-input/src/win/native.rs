//! Off-path discovery. Never enables detours, acquires game devices, or calls game functions.
use super::abi::{OwnedCom, Raw, directinput as di, gameinput as gi};
use crate::native::*;
use std::ffi::CStr;
use windows_sys::{
    Win32::{Foundation::GetLastError, System::LibraryLoader::*},
    core::{GUID, w},
};
static OWNERSHIP: OwnershipGate = OwnershipGate::new();
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
static PENDING: AtomicBool = AtomicBool::new(true);
static READY: AtomicBool = AtomicBool::new(false);
static COVERAGE_PENDING: AtomicBool = AtomicBool::new(true);
static DISCOVERY: Mutex<Option<DiscoveryReport>> = Mutex::new(None);
#[repr(C)]
struct UnicodeName {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}
#[repr(C)]
struct Notification {
    flags: u32,
    full_name: *const UnicodeName,
    base_name: *const UnicodeName,
    base: *mut core::ffi::c_void,
    size: u32,
}
unsafe extern "system" fn loaded(reason: u32, data: *const Notification, context: *mut core::ffi::c_void) {
    if !matches!(reason, 1 | 2) || data.is_null() || context.is_null() {
        return;
    }
    // SAFETY: the loader supplies its notification and name for this callback's duration.
    let name = unsafe { (*data).base_name };
    if name.is_null() {
        return;
    }
    // SAFETY: nonnull loader-owned UNICODE_STRING, with a byte length and UTF-16 buffer.
    let name = unsafe { &*name };
    if name.buffer.is_null() || name.length % 2 != 0 || name.length > name.maximum_length {
        return;
    }
    // SAFETY: the loader guarantees Length readable bytes in Buffer during notification.
    let units = unsafe { std::slice::from_raw_parts(name.buffer, name.length as usize / 2) };
    let matches =
        |ascii: &[u8]| units.len() == ascii.len() && units.iter().zip(ascii).all(|(u, a)| (*u | 32) == *a as u16);
    if !matches(b"dinput8.dll") && !matches(b"gameinput.dll") {
        return;
    }
    READY.store(false, Ordering::Release);
    PENDING.store(true, Ordering::Release);
    // SAFETY: context is Arc::as_ptr of the process-resident Shared stored in SHARED.
    let shared = unsafe { &*(context as *const super::Shared) };
    shared.permit.store(false, Ordering::Release);
    shared.release.store(uw_pipe_proto::input::InputReason::Unavailable as usize + 1, Ordering::Release);
    revoke(uw_pipe_proto::input::InputReason::Unavailable);
}
pub(super) fn observe_modules(shared: &super::Shared) -> Result<(), super::InstallError> {
    type Register = unsafe extern "system" fn(
        u32,
        Option<unsafe extern "system" fn(u32, *const Notification, *mut core::ffi::c_void)>,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> i32;
    // SAFETY: ntdll is process-resident; the terminated export and signature follow Microsoft's devnote.
    let function =
        unsafe { GetProcAddress(GetModuleHandleW(w!("ntdll.dll")), c"LdrRegisterDllNotification".as_ptr().cast()) }
            .ok_or_else(|| super::InstallError("DLL input-runtime observation unavailable".into()))?;
    // SAFETY: exact dynamically resolved registration ABI.
    let register: Register = unsafe { std::mem::transmute(function) };
    let mut cookie = std::ptr::null_mut();
    // SAFETY: callback and Shared outlive the process; cookie is writable, flags must be zero.
    let status = unsafe { register(0, Some(loaded), (shared as *const super::Shared).cast_mut().cast(), &mut cookie) };
    if status < 0 {
        return Err(super::InstallError(format!("DLL input-runtime observation failed: {status:#x}")));
    }
    Ok(())
}
pub(super) fn ready() -> bool {
    READY.load(Ordering::Acquire)
        && !PENDING.load(Ordering::Acquire)
        && super::directinput::healthy()
        && super::lowlevel::ready()
}
pub(super) fn request_coverage() {
    COVERAGE_PENDING.store(true, Ordering::Release);
}
/// Watchdog only. No discovery, allocation, locks or OS calls occur in the notification callback.
pub(super) fn poll(shared: &super::Shared) {
    super::directinput::drain_deferred();
    if PENDING.swap(false, Ordering::AcqRel) {
        super::window::wake(shared);
        let mut report = discover();
        if report.directinput.status == DiscoveryStatus::Discovered {
            report.directinput = super::directinput::install(&report).unwrap_or_else(|error| {
                report.diagnostics.push(error.0);
                AdapterCoverage {
                    status: DiscoveryStatus::Failed,
                    abi_mask: report.directinput.abi_mask,
                    detail: 0x8000_ffffu32 as i32,
                }
            });
        }
        let ready = report.capture_available();
        if let Ok(mut current) = DISCOVERY.lock() {
            *current = Some(report);
        }
        READY.store(ready, Ordering::Release);
        COVERAGE_PENDING.store(true, Ordering::Release);
    }
    if !COVERAGE_PENDING.load(Ordering::Acquire) || !shared.routing_mode.load(Ordering::Acquire) {
        return;
    }
    let metadata = shared.policy.try_lock().ok().map(|p| (p.epoch(), p.router.revision()));
    let Some((epoch, revision)) = metadata else {
        return;
    };
    if revision == 0 {
        return;
    }
    let coverage = DISCOVERY.lock().ok().and_then(|r| {
        r.as_ref().map(|r| uw_pipe_proto::routing::NativeCoverage {
            epoch,
            revision,
            directinput: api_coverage(if !super::directinput::healthy() {
                AdapterCoverage { status: DiscoveryStatus::Failed, ..r.directinput }
            } else if !super::lowlevel::ready() && r.directinput.status == DiscoveryStatus::Supported {
                AdapterCoverage { status: DiscoveryStatus::Discovered, ..r.directinput }
            } else {
                r.directinput
            }),
            gameinput: api_coverage(r.gameinput),
        })
    });
    if let Some(coverage) = coverage {
        COVERAGE_PENDING.store(false, Ordering::Release);
        shared.emit(super::RuntimeEvent::NativeCoverage(coverage));
    }
}
fn api_coverage(coverage: AdapterCoverage) -> uw_pipe_proto::routing::ApiCoverage {
    use uw_pipe_proto::routing::{ApiCoverage, Domains, NativeStatus};
    let status = match coverage.status {
        DiscoveryStatus::Absent => NativeStatus::Absent,
        DiscoveryStatus::Discovered => NativeStatus::Installing,
        DiscoveryStatus::Supported => NativeStatus::Supported,
        DiscoveryStatus::Unsupported => NativeStatus::Unsupported,
        DiscoveryStatus::Failed => NativeStatus::Failed,
    };
    ApiCoverage {
        status,
        domains: if status == NativeStatus::Supported {
            Domains { keyboard: true, mouse: true }
        } else {
            Domains::default()
        },
        abi_mask: coverage.abi_mask,
        detail: coverage.detail as u32,
    }
}
pub fn publish_ownership(epoch: u64, domains: uw_pipe_proto::routing::Domains, exclusive: bool) -> bool {
    let ticket = OWNERSHIP.ticket();
    let Some(shared) = super::SHARED.get() else {
        return false;
    };
    if shared.release.load(Ordering::Acquire) != 0
        || !shared.permit.load(Ordering::Acquire)
        || shared.epoch.load(Ordering::Acquire) != epoch
    {
        return false;
    }
    OWNERSHIP.publish(ticket, epoch, domains, exclusive)
}
/// Loader callbacks may call this: atomics only, no wake, allocation, lock or system call.
pub fn revoke(_reason: uw_pipe_proto::input::InputReason) {
    OWNERSHIP.revoke();
}
pub fn ownership() -> (u64, uw_pipe_proto::routing::Domains, bool) {
    OWNERSHIP.snapshot()
}

/// Resolve a loaded system DLL and retain it for the process lifetime before publishing addresses.
pub fn resolve_system_export(module: &CStr, export: &CStr) -> Result<usize, i32> {
    let wide: Vec<u16> = module.to_bytes().iter().map(|byte| *byte as u16).chain(Some(0)).collect();
    let mut handle = std::ptr::null_mut();
    // SAFETY: terminated module name and writable handle local; no game pointers are dereferenced.
    if unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, wide.as_ptr(), &mut handle) } == 0 {
        // SAFETY: reads the calling thread's last-error value.
        return Err(unsafe { GetLastError() } as i32);
    }
    // SAFETY: the system module is pinned, and export is NUL-terminated.
    unsafe { GetProcAddress(handle, export.as_ptr().cast()) }
        .map(|function| function as usize)
        // SAFETY: reads the failed lookup's last error immediately.
        .ok_or_else(|| unsafe { GetLastError() } as i32)
}

fn target(
    report: &mut DiscoveryReport,
    registry: &mut TargetRegistry,
    api: NativeApi,
    abi: u16,
    method: &'static str,
    address: usize,
    signature: u16,
) -> Result<(), i32> {
    registry.insert(address, SignatureId(signature)).map_err(|error| {
        report.diagnostics.push(format!("{api:?} {abi} {method}: {error:?}"));
        0x8000_ffffu32 as i32
    })?;
    report.targets.push(DiscoveryTarget { api, abi, method, address, signature: SignatureId(signature) });
    Ok(())
}

pub fn discover() -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let mut registry = TargetRegistry::new();
    // SAFETY: only checks loaded modules; discovery does not load optional runtimes into a game.
    if !unsafe { GetModuleHandleW(w!("dinput8.dll")) }.is_null() {
        match discover_directinput(&mut report, &mut registry) {
            Ok(mask) => {
                report.directinput = AdapterCoverage { status: DiscoveryStatus::Discovered, abi_mask: mask, detail: 0 }
            }
            Err(detail) => report.directinput = AdapterCoverage::failure(detail),
        }
    }
    // SAFETY: this query does not load or initialize GameInput.
    if !unsafe { GetModuleHandleW(w!("GameInput.dll")) }.is_null() {
        match discover_gameinput(&mut report, &mut registry) {
            Ok(mask) if mask != 0 => {
                report.gameinput = AdapterCoverage { status: DiscoveryStatus::Discovered, abi_mask: mask, detail: 0 }
            }
            Ok(_) => {
                report.gameinput = AdapterCoverage { status: DiscoveryStatus::Unsupported, abi_mask: 0, detail: 0 }
            }
            Err(detail) => report.gameinput = AdapterCoverage::failure(detail),
        }
    }
    report
}

fn discover_directinput(report: &mut DiscoveryReport, registry: &mut TargetRegistry) -> Result<u16, i32> {
    let address = resolve_system_export(c"dinput8.dll", c"DirectInput8Create")?;
    target(report, registry, NativeApi::DirectInput, 0, "DirectInput8Create", address, 10)?;
    // SAFETY: documented DirectInput8Create export resolved by its real address.
    let create = unsafe { std::mem::transmute::<usize, di::Create>(address) };
    let mut mask = 0;
    for (abi, iid) in [(1, di::IID_A), (2, di::IID_W)] {
        let mut factory: Raw = std::ptr::null_mut();
        // SAFETY: current executable instance, DirectInput version 8, exact IID and writable output.
        let hr =
            unsafe { create(GetModuleHandleW(std::ptr::null()), 0x0800, &iid, &mut factory, std::ptr::null_mut()) };
        if hr < 0 {
            return Err(hr);
        }
        // SAFETY: a successful system factory returns one owned COM reference.
        let factory = unsafe { OwnedCom::from_owned(factory) }.ok_or(0x8000_4003u32 as i32)?;
        let unknown = factory.query(&GUID::from_u128(0x00000000_0000_0000_c000_000000000046))?;
        for (index, name, signature) in
            [(0, "FactoryUnknownQuery", 1), (1, "FactoryUnknownAddRef", 2), (2, "FactoryUnknownRelease", 3)]
        {
            // SAFETY: QI(IUnknown) may return a different factory prefix; it still has exactly these slots.
            let address = unsafe { unknown.slot(index) };
            target(report, registry, NativeApi::DirectInput, abi, name, address, signature)?;
        }
        for (index, name, signature) in
            [(0, "FactoryQueryInterface", 1), (1, "FactoryAddRef", 2), (2, "FactoryRelease", 3)]
        {
            // SAFETY: both successfully selected factory IIDs have the IUnknown prefix.
            let address = unsafe { factory.slot(index) };
            target(report, registry, NativeApi::DirectInput, abi, name, address, signature)?;
        }
        // SAFETY: verified DirectInput 8A/W interface has CreateDevice at slot 3.
        let create_address = unsafe { factory.slot(3) };
        target(report, registry, NativeApi::DirectInput, abi, "CreateDevice", create_address, 11)?;
        // SAFETY: this is the exact SDK signature for the verified factory IID.
        let create_device = unsafe { std::mem::transmute::<usize, di::CreateDevice>(create_address) };
        for guid in [
            windows_sys::Win32::Devices::HumanInterfaceDevice::GUID_SysKeyboard,
            windows_sys::Win32::Devices::HumanInterfaceDevice::GUID_SysMouse,
        ] {
            let mut device: Raw = std::ptr::null_mut();
            // SAFETY: valid own factory, built-in device GUID and writable output; device is not acquired.
            let hr = unsafe { create_device(factory.raw(), &guid, &mut device, std::ptr::null_mut()) };
            if hr < 0 {
                return Err(hr);
            }
            // SAFETY: successful CreateDevice returns an owned interface.
            let device = unsafe { OwnedCom::from_owned(device) }.ok_or(0x8000_4003u32 as i32)?;
            for &(slot, name, signature) in di::METHODS {
                // SAFETY: all listed slots are specified by both verified DirectInput 8 interfaces.
                let address = unsafe { device.slot(slot) };
                target(report, registry, NativeApi::DirectInput, abi, name, address, signature)?;
            }
        }
        mask |= abi;
    }
    Ok(mask)
}

fn discover_gameinput(report: &mut DiscoveryReport, registry: &mut TargetRegistry) -> Result<u16, i32> {
    let address = resolve_system_export(c"GameInput.dll", c"GameInputCreate")?;
    target(report, registry, NativeApi::GameInput, 0, "GameInputCreate", address, 99)?;
    // SAFETY: GameInputCreate's export returns the legacy interface, regardless of the header alias.
    let create = unsafe { std::mem::transmute::<usize, gi::Create>(address) };
    let mut legacy: Raw = std::ptr::null_mut();
    // SAFETY: writable local output for the documented factory.
    let hr = unsafe { create(&mut legacy) };
    if hr < 0 {
        return Err(hr);
    }
    // SAFETY: successful system factory owns one reference.
    let legacy = unsafe { OwnedCom::from_owned(legacy) }.ok_or(0x8000_4003u32 as i32)?;
    if legacy.query(&GUID::from_u128(u128::MAX)).is_ok() {
        report.diagnostics.push("GameInput accepted an unknown IID; no layout can be trusted".into());
        return Err(0x8000_ffffu32 as i32);
    }
    let mut mask = 0;
    for (version, iid) in gi::IIDS.iter().enumerate() {
        let interface = match legacy.query(iid) {
            Ok(value) => value,
            Err(hr) => {
                report.diagnostics.push(format!("GameInput v{version} QueryInterface: {hr:#x}"));
                continue;
            }
        };
        // SAFETY: QI selected this exact documented interface; timestamp is slot 3 in all four ABIs.
        let clock = unsafe { std::mem::transmute::<usize, gi::Timestamp>(interface.slot(3)) };
        // SAFETY: the object remains owned through both timestamp queries.
        let (before, after) = unsafe { (clock(interface.raw()), clock(interface.raw())) };
        if after < before {
            return Err(0x8000_ffffu32 as i32);
        }
        let methods = if version == 0 { gi::V0_METHODS } else { gi::NEW_METHODS };
        for &(slot, name, signature) in methods {
            // SAFETY: version-specific method slots come from the pinned public header for this QI IID.
            let address = unsafe { interface.slot(slot) };
            target(report, registry, NativeApi::GameInput, 1 << version, name, address, signature)?;
        }
        mask |= 1 << version;
        report.diagnostics.push(format!(
            "GameInput v{version}: timestamp {before}..{after}; originals discovered, isolation untested"
        ));
    }
    Ok(mask)
}
