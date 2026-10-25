//! Off-path discovery. Never enables detours, acquires game devices, or calls game functions.
use super::abi::{OwnedCom, Raw, directinput as di, gameinput as gi};
use crate::native::*;
use std::ffi::CStr;
use windows_sys::{
    Win32::{Foundation::GetLastError, System::LibraryLoader::*},
    core::{GUID, w},
};

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
