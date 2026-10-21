//! Runs only for the windows target (under Wine). Needs both DLLs built first:
//! `cargo build --target x86_64-pc-windows-gnu -p uw-spike-input -p uw-spike-dlcollector`, then
//! `cargo test --target x86_64-pc-windows-gnu -p uw-spike-input --test win_module --no-run` and
//! run the exe with `wine`. Plays the host: loads the module DLL, answers its `Hello`, and asks it
//! to load the collector plugin.
#![cfg(windows)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use uw_spike_ipc::{Msg, read_msg, write_msg};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::core::PCWSTR;

fn dll(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join(name)
}

#[test]
fn module_loads_plugin_on_command() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    // SAFETY: set before the module's attach thread reads it; no other thread reads the env here.
    unsafe { std::env::set_var("UW_SPIKE_HOST_PORT", port.to_string()) };

    let module = dll("uw_spike_input.dll");
    let wide: Vec<u16> = module.to_str().unwrap().encode_utf16().chain(Some(0)).collect();
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.unwrap_or_else(|e| panic!("{}: {e}", module.display()));

    let (mut s, _) = listener.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert!(matches!(read_msg(&mut s).unwrap(), Msg::Hello { .. }));
    write_msg(&mut s, &Msg::ModuleConfig { input_hooks: false }).unwrap();

    let missing = dll("no_such_plugin.dll").to_str().unwrap().to_string();
    write_msg(&mut s, &Msg::LoadPlugin { path: missing.clone(), channel: 900_201, args: String::new() }).unwrap();
    let Msg::PluginLoaded { path, ok, error } = read_msg(&mut s).unwrap() else { panic!("expected PluginLoaded") };
    assert_eq!((path, ok), (missing, false));
    assert!(error.unwrap().starts_with("LoadLibraryW failed"));

    let collector = dll("uw_spike_dlcollector.dll").to_str().unwrap().to_string();
    let load = Msg::LoadPlugin { path: collector.clone(), channel: 900_201, args: "discover=1".into() };
    write_msg(&mut s, &load).unwrap();
    assert_eq!(read_msg(&mut s).unwrap(), Msg::PluginLoaded { path: collector.clone(), ok: true, error: None });
    write_msg(&mut s, &load).unwrap();
    assert_eq!(
        read_msg(&mut s).unwrap(),
        Msg::PluginLoaded { path: collector, ok: false, error: Some("already loaded".into()) }
    );
}
