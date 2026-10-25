use uw_input::native::{DiscoveryStatus, NativeApi};
use uw_input::win::native;
use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_sys::core::w;

pub fn run() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|a| a == "--late-load") {
        let initial = native::discover();
        assert_eq!(initial.directinput.status, DiscoveryStatus::Absent);
        assert_eq!(initial.gameinput.status, DiscoveryStatus::Absent);
    }
    // SAFETY: fixed system DLL names; this standalone probe owns its process, not a game prefix.
    unsafe {
        LoadLibraryExW(w!("dinput8.dll"), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        LoadLibraryExW(w!("GameInput.dll"), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    assert!(native::resolve_system_export(c"kernel32.dll", c"UnderwolfMissingExport").is_err());
    assert!(native::resolve_system_export(c"underwolf-missing-system-module.dll", c"Unknown").is_err());
    let preexisting = super::directinput::Devices::create();
    let gameinput_result = super::gameinput::factory_result();
    let report = native::discover();
    if let Some(devices) = &preexisting {
        devices.verify_unchanged();
    }
    if let Some(result) = gameinput_result.filter(|result| *result < 0) {
        assert_eq!(report.gameinput.detail, result, "adapter and independent factory dispatch disagree");
    }
    for target in &report.targets {
        assert_ne!(target.address, 0, "{}", target.method);
    }
    println!(
        "{{\"directinput\":\"{:?}\",\"directinput_abis\":{},\"directinput_detail\":{},\"gameinput\":\"{:?}\",\"gameinput_abis\":{},\"gameinput_detail\":{},\"targets\":{}}}",
        report.directinput.status,
        report.directinput.abi_mask,
        report.directinput.detail,
        report.gameinput.status,
        report.gameinput.abi_mask,
        report.gameinput.detail,
        report.targets.len()
    );
    for line in &report.diagnostics {
        eprintln!("{line}");
    }
    for target in &report.targets {
        eprintln!(
            "{:?} abi={} {} address={:#x} signature={}",
            target.api, target.abi, target.method, target.address, target.signature.0
        );
    }
    if args.iter().any(|a| a == "--directinput") {
        assert_eq!(report.directinput.status, DiscoveryStatus::Discovered);
        assert!(report.targets.iter().any(|t| t.api == NativeApi::DirectInput));
    }
    if args.iter().any(|a| a == "--gameinput") {
        assert_eq!(report.gameinput.status, DiscoveryStatus::Discovered);
        assert!(report.targets.iter().any(|t| t.api == NativeApi::GameInput));
    }
}
