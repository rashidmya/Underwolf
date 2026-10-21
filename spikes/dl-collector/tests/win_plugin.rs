//! Runs only for the windows target (under Wine), like `win_timing`. Starts the plugin through its
//! exported ABI in discovery mode and reads the channel as the helper would. Creates a window, so
//! it needs a display Wine can use: `xvfb-run -a wine <exe>`.
#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use uw_spike_dlcollector as _;
use uw_spike_gep_channel::InfoReader;
use uw_spike_gep_channel::win::Consumer;
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, WINDOW_EX_STYLE,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::w;

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

#[inline(never)]
fn qpc_on_window_thread() {
    let mut v = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
}

#[inline(never)]
fn qpc_on_test_thread() {
    let mut v = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
}

/// A thread that owns a visible top-level window and calls QPC while it pumps messages.
fn window_thread(stop: Arc<AtomicBool>) -> (u32, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("uw discovery test"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                200,
                100,
                None,
                None,
                None,
                None,
            )
        };
        assert!(hwnd.is_ok(), "CreateWindowExW: {hwnd:?}");
        tx.send(unsafe { GetCurrentThreadId() }).unwrap();
        let mut msg = MSG::default();
        while !stop.load(Ordering::Relaxed) {
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe { DispatchMessageW(&msg) };
            }
            qpc_on_window_thread();
        }
    });
    (rx.recv().unwrap(), t)
}

#[test]
fn discovery_publishes_the_busiest_qpc_chains() {
    let ch = 900_101;
    let stop = Arc::new(AtomicBool::new(false));
    let (window_tid, window) = window_thread(stop.clone());
    let me = unsafe { GetCurrentThreadId() };
    assert_ne!(window_tid, me);
    let file = std::env::temp_dir().join(format!("uw-discovery-{}.json", std::process::id()));
    let args = format!("discover=1 discover_out={}", file.display());
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
        qpc_on_test_thread();
    }
    unsafe { uw_plugin_present() };
    stop.store(true, Ordering::Relaxed);
    window.join().unwrap();
    assert_eq!(value(&c, "plugin_status").as_deref(), Some("discovered"));
    let json = value(&c, "discovery").unwrap();
    eprintln!("discovery: {json}");
    assert!(json.len() <= 4096);
    assert!(json.starts_with(r#"{"dropped_busy":"#), "{json}");
    assert!(
        json.contains(r#""chains":[{"frames":[{"module":"#),
        "{json}"
    );
    assert!(
        json.contains("win_plugin-"),
        "the test exe's own call site is listed: {json}"
    );
    assert!(json.contains(r#""window_thread":false"#), "{json}");
    assert_eq!(value(&c, "plugin_error"), None);

    // The full table: every chain, with its threads and the window flag.
    let table = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    eprintln!("table: {}", &table[..table.len().min(1500)]);
    assert!(
        table.starts_with(r#"{"seconds":"#)
            && table.contains(&format!(r#""window_thread_id":{window_tid},"#)),
        "{}",
        &table[..table.len().min(300)]
    );
    let chains: Vec<&str> = table
        .lines()
        .filter(|l| l.starts_with(r#"{"frames""#))
        .collect();
    assert!(chains.len() > 32 || json.matches("per_sec").count() == chains.len());
    let on = |tid: u32| format!(r#""threads":[{{"tid":{tid},"#);
    let window_chains: Vec<_> = chains
        .iter()
        .filter(|l| l.contains(&on(window_tid)))
        .collect();
    let test_chains: Vec<_> = chains.iter().filter(|l| l.contains(&on(me))).collect();
    assert!(
        !window_chains.is_empty() && !test_chains.is_empty(),
        "{table}"
    );
    assert!(
        window_chains
            .iter()
            .all(|l| l.contains(r#""window_thread":true"#)),
        "{window_chains:?}"
    );
    assert!(
        test_chains
            .iter()
            .all(|l| l.contains(r#""window_thread":false"#)),
        "{test_chains:?}"
    );
    // Five frames are recorded where the stack is that deep.
    assert!(chains.iter().any(|l| {
        l.split_once(r#""calls""#)
            .unwrap()
            .0
            .matches(r#""rva""#)
            .count()
            == 5
    }));
}
