//! Runs only for the windows target (under Wine), like `win_timing`. Outside the game there is no
//! `client.dll`: the plugin starts, publishes the error and arms nothing.
#![cfg(windows)]

use uw_spike_dlcollector as _;
use uw_spike_gep_channel::InfoReader;
use uw_spike_gep_channel::win::Consumer;

unsafe extern "C" {
    fn uw_plugin_start(channel: u32, args: *const u8, args_len: usize) -> i32;
    fn uw_plugin_present();
}

#[test]
fn missing_client_module_publishes_error() {
    let ch = 900_102;
    assert_eq!(unsafe { uw_plugin_start(ch, std::ptr::null(), 0) }, 0);
    unsafe { uw_plugin_present() };
    let c = Consumer::open(ch).unwrap();
    let snap = c
        .with_info(|r| InfoReader::attach(r).unwrap().snapshot(r))
        .unwrap();
    let get = |k: &str| {
        snap.iter()
            .find(|ch| ch.category == "gep_internal" && ch.key == k)
            .and_then(|ch| ch.value.clone())
            .map(|v| String::from_utf8(v).unwrap())
    };
    assert_eq!(get("plugin_status").as_deref(), Some("error"));
    assert_eq!(
        get("plugin_error").as_deref(),
        Some("client.dll not loaded")
    );
}
