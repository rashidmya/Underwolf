//! Runs only for the windows target (under Wine): build with
//! `cargo test --target x86_64-pc-windows-gnu -p uw-spike-dlcollector --test win_timing --no-run`
//! and run the exe with `wine`. Installs the real MinHook QPC hook in the test process.
#![cfg(windows)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use uw_spike_dlcollector::timing::{DISCOVERY_FRAMES, QpcSink, install};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::core::{s, w};

static QPC: AtomicUsize = AtomicUsize::new(0);

/// Calls `kernel32!QueryPerformanceCounter` from a call site with a known return address:
/// `sub rsp, 40` is 4 bytes and `call [rip+rel32]` 6, so the return address is `probe + 10`.
/// The SEH directives give it unwind info, so a stack walk can pass through it.
#[unsafe(naked)]
unsafe extern "system" fn probe(_counter: *mut i64) -> i32 {
    core::arch::naked_asm!(
        ".seh_proc {me}",
        "sub rsp, 40",
        ".seh_stackalloc 40",
        ".seh_endprologue",
        "call qword ptr [rip + {qpc}]",
        "add rsp, 40",
        "ret",
        ".seh_endproc",
        me = sym probe,
        qpc = sym QPC,
    )
}

#[derive(Default)]
struct Seen {
    calls: Vec<(u64, Option<u64>)>,
}

struct Recorder(Mutex<Seen>);

impl QpcSink for Recorder {
    fn main_loop_call(&self, ret: u64, outer: &dyn Fn() -> Option<u64>) {
        let o = outer();
        self.0.lock().unwrap().calls.push((ret, o));
    }
    fn discovery_call(&self, _frames: [u64; DISCOVERY_FRAMES]) {
        panic!("not in discovery mode");
    }
}

static SINK: Recorder = Recorder(Mutex::new(Seen { calls: Vec::new() }));

#[inline(never)]
fn call_probe() -> (i32, i64) {
    let mut v = 0i64;
    let r = unsafe { probe(&mut v) };
    (r, v)
}

#[test]
fn hook_runs_only_for_the_main_loop_site_and_is_transparent() {
    let qpc = unsafe {
        let k32 = GetModuleHandleW(w!("kernel32.dll")).unwrap();
        GetProcAddress(k32, s!("QueryPerformanceCounter")).unwrap()
    };
    QPC.store(qpc as usize, Ordering::Relaxed);
    let site = probe as *const () as u64 + 10;
    install(&SINK, Some(site)).unwrap();
    assert!(install(&SINK, Some(site)).is_err(), "installs once");

    let mut last = 0i64;
    for _ in 0..10_000 {
        let mut v = 0i64;
        assert!(unsafe { QueryPerformanceCounter(&mut v) }.is_ok());
        assert!(v >= last);
        last = v;
    }
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..10_000 {
                    let mut v = 0i64;
                    let _ = unsafe { QueryPerformanceCounter(&mut v) };
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(
        SINK.0.lock().unwrap().calls.is_empty(),
        "non-matching callers reached the sink"
    );

    for _ in 0..3 {
        let (r, v) = call_probe();
        assert_ne!(r, 0);
        assert!(v >= last);
        last = v;
    }
    let calls = std::mem::take(&mut SINK.0.lock().unwrap().calls);
    assert_eq!(calls.len(), 3, "{calls:x?}");
    let here = call_probe as *const () as u64;
    for (ret, outer) in calls {
        assert_eq!(ret, site);
        // The frame above the probe is `call_probe`.
        let outer = outer.expect("outer frame");
        assert!(
            outer > here && outer < here + 0x200,
            "{outer:#x} vs call_probe {here:#x}"
        );
    }
}
