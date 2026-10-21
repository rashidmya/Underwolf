//! Runs only for the windows target (under Wine), like `win_timing`. Starts the plugin through its
//! exported ABI in discovery mode and reads the channel as the helper would.
#![cfg(windows)]

use std::time::{Duration, Instant};

use uw_spike_dlcollector as _;
use uw_spike_gep_channel::InfoReader;
use uw_spike_gep_channel::win::Consumer;
use windows::Win32::System::Performance::QueryPerformanceCounter;

unsafe extern "C" {
    fn uw_plugin_start(channel: u32, args: *const u8, args_len: usize) -> i32;
    fn uw_plugin_present();
    fn uw_plugin_stop() -> i32;
}

fn value(c: &Consumer, key: &str) -> Option<String> {
    let snap = c
        .with_info(|r| InfoReader::attach(r).unwrap().snapshot(r))
        .unwrap();
    snap.into_iter()
        .find(|ch| ch.category == "gep_internal" && ch.key == key)
        .and_then(|ch| ch.value)
        .map(|v| String::from_utf8(v).unwrap())
}

#[test]
fn discovery_publishes_the_busiest_qpc_chains() {
    let ch = 900_101;
    let args = "discover=1";
    assert_eq!(unsafe { uw_plugin_start(ch, args.as_ptr(), args.len()) }, 0);
    assert_eq!(
        unsafe { uw_plugin_start(ch, args.as_ptr(), args.len()) },
        2,
        "starts once"
    );
    assert_eq!(unsafe { uw_plugin_stop() }, 1, "refuses unload");
    let c = Consumer::open(ch).unwrap();
    assert_eq!(value(&c, "plugin_status").as_deref(), Some("discovering"));

    let end = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < end {
        let mut v = 0i64;
        let _ = unsafe { QueryPerformanceCounter(&mut v) };
    }
    unsafe { uw_plugin_present() };
    assert_eq!(value(&c, "plugin_status").as_deref(), Some("discovered"));
    let json = value(&c, "discovery").unwrap();
    eprintln!("discovery: {json}");
    assert!(json.len() <= 4096);
    assert!(json.starts_with(r#"[{"frames":[{"module":"#), "{json}");
    assert!(
        json.contains("win_plugin-"),
        "the test exe's own call site is listed: {json}"
    );
    assert!(json.contains(r#""window_thread":false"#), "{json}");
}
