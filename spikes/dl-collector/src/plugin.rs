//! Plugin ABI of `uw_spike_dlcollector.dll` (`cfg(windows)`). The in-game module loads the DLL,
//! calls `uw_plugin_start` once, then `uw_plugin_present` about every 16 ms from its own thread.
//!
//! Passes run on the game's main thread, inside the QPC hook (`timing`). Everything the main
//! thread touches sits behind one mutex that it only `try_lock`s: when the present thread holds it,
//! that QPC call simply skips its pass. The channel's named mutexes are also only tried, never
//! waited on, so the main thread never blocks on another process.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use uw_spike_gep_channel::win::Producer;
use uw_spike_gep_channel::{ChannelError, InfoWriter, Throttle, ring_push};
use windows::Win32::Foundation::{BOOL, HMODULE, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::PCWSTR;

use crate::collector::{ChannelOut, Collector};
use crate::keys::{DISCOVERY_MAX, GEP_INTERNAL, PLUGIN_ERROR_MAX, deadlock_schema};
use crate::mem::SelfProcess;
use crate::module::Module;
use crate::roots::resolve_main_loop;
use crate::timing::{
    self, DISCOVERY_FRAMES, DISCOVERY_TOP, DiscoverArgs, DiscoveryTotals, Frame, QpcSink,
    SharedDiscovery, TickGate, chains_from, discovery_file_json, discovery_json,
};

const MODULES: [&str; 3] = ["client.dll", "engine2.dll", "tier0.dll"];
const EVENT_BURST: usize = 30;
const EVENT_WINDOW_DIV: u64 = 5;
/// While discovering, the window's thread is looked up again every this many presents (~1 s).
const WINDOW_REFRESH_PRESENTS: u32 = 60;

type Sets = BTreeMap<(String, String), Option<Vec<u8>>>;

/// The channel producer as a `ChannelOut`: `commit` = one `Txn` + one info signal; `push_event` =
/// `Throttle::admit` then `ring_push`. A commit that finds the info mutex held by the helper is
/// kept and merged into the next attempt (the next commit or `flush` from the present thread).
struct ProducerOut {
    producer: Producer,
    throttle: Throttle,
    pending: Sets,
    pending_ts: u64,
}

impl ProducerOut {
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let (pending, ts) = (&self.pending, self.pending_ts);
        let written = self.producer.with_info(|region| {
            let Ok(mut w) = InfoWriter::attach(region) else {
                return false;
            };
            let mut txn = w.begin();
            for ((cat, key), value) in pending {
                // The collector already drops unknown keys and over-long values.
                let _ = txn.set(cat, key, value.as_deref(), ts);
            }
            txn.commit()
        });
        // `None`: the helper holds the mutex; keep the sets and retry later.
        if let Some(changed) = written {
            self.pending.clear();
            if changed {
                self.producer.signal_info();
            }
        }
    }

    fn commit(&mut self, sets: &[(String, String, Option<Vec<u8>>)], ts: u64) {
        for (cat, key, value) in sets {
            self.pending
                .insert((cat.clone(), key.clone()), value.clone());
        }
        self.pending_ts = ts;
        self.flush();
    }

    fn push_event(&mut self, name: &str, data: &[u8], ts: u64) -> bool {
        if !self.throttle.admit(qpc_now()) {
            return false;
        }
        match self
            .producer
            .with_events(|region| ring_push(region, name, data, ts))
        {
            None => false,
            Some(Ok(())) => {
                self.producer.signal_events();
                true
            }
            // Too long for a ring record: retrying would never succeed, so drop it.
            Some(Err(ChannelError::TooLong)) => true,
            Some(Err(_)) => false,
        }
    }

    fn status(&mut self, status: &str, error: Option<&str>) {
        let mut sets = vec![(
            GEP_INTERNAL.to_string(),
            "plugin_status".to_string(),
            Some(status.as_bytes().to_vec()),
        )];
        if let Some(e) = error {
            let mut end = e.len().min(PLUGIN_ERROR_MAX as usize);
            while !e.is_char_boundary(end) {
                end -= 1;
            }
            sets.push((
                GEP_INTERNAL.into(),
                "plugin_error".into(),
                Some(e.as_bytes()[..end].to_vec()),
            ));
        }
        self.commit(&sets, ms_now());
    }
}

/// The collector's handle on the shared producer. Uncontended: every user holds `PLUGIN` first.
struct OutHandle(Arc<Mutex<ProducerOut>>);

impl OutHandle {
    fn lock(&self) -> MutexGuard<'_, ProducerOut> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ChannelOut for OutHandle {
    fn commit(&mut self, sets: &[(String, String, Option<Vec<u8>>)], ts: u64) {
        self.lock().commit(sets, ts);
    }

    fn push_event(&mut self, name: &str, data: &[u8], ts: u64) -> bool {
        self.lock().push_event(name, data, ts)
    }
}

struct Discovering {
    started: u64,
    deadline: u64,
    done: bool,
    /// `discover_out=`: where the full chain table goes.
    out_path: Option<String>,
    presents: u32,
}

enum Mode {
    /// Passes are not armed (start failed or the main-loop caller is unknown).
    Idle,
    Collecting {
        collector: Box<Collector>,
        gate: TickGate,
    },
    Discovering(Discovering),
    /// Started with `selftest=1`: the collector published its status and one event, and reads
    /// nothing (no game modules are needed, so it also runs in a plain Wine process).
    Selftest(Box<Collector>),
}

struct Plugin {
    out: OutHandle,
    mode: Mode,
}

// SAFETY: `Plugin` is only reached through the `PLUGIN` mutex. It holds no thread-affine state:
// the producer's handles and views are process-wide, and the collector owns plain data, its
// `OutHandle` and a `Send` clock.
unsafe impl Send for Plugin {}

static PLUGIN: Mutex<Option<Plugin>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);
static CHAINS: SharedDiscovery = SharedDiscovery::new();

fn qpc_now() -> u64 {
    let mut v = 0i64;
    // SAFETY: writes one i64.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v as u64
}

fn qpc_freq() -> u64 {
    let mut v = 0i64;
    // SAFETY: writes one i64.
    let _ = unsafe { QueryPerformanceFrequency(&mut v) };
    (v as u64).max(1)
}

fn ms_now() -> u64 {
    u64::try_from(u128::from(qpc_now()) * 1000 / u128::from(qpc_freq())).unwrap_or(u64::MAX)
}

fn lock_plugin() -> MutexGuard<'static, Option<Plugin>> {
    PLUGIN.lock().unwrap_or_else(|p| p.into_inner())
}

struct Sink;

impl QpcSink for Sink {
    fn main_loop_call(&self, ret: u64, outer: &dyn Fn() -> Option<u64>) {
        let mut guard = match PLUGIN.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let Some(Plugin {
            mode: Mode::Collecting { collector, gate },
            ..
        }) = guard.as_mut()
        else {
            return;
        };
        if !gate.should_run(ret, qpc_now(), outer) {
            return;
        }
        collector.tick(&SelfProcess);
        gate.done();
    }

    fn discovery_call(&self, frames: [u64; DISCOVERY_FRAMES]) {
        // SAFETY: no arguments, no side effects.
        let thread = unsafe { GetCurrentThreadId() };
        // Never waits: a held lock drops (and counts) this call.
        CHAINS.record(frames, thread);
    }
}

static SINK: Sink = Sink;

/// `selftest=1` (the collector's own test mode), if present.
fn selftest(args: &str) -> bool {
    args.split_whitespace().any(|t| t == "selftest=1")
}

fn loaded_modules() -> Vec<Module> {
    MODULES
        .iter()
        .filter_map(|name| {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: NUL-terminated name; the handle is not reference-counted.
            let h = unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())) }.ok()?;
            Module::from_headers(&SelfProcess, name, h.0 as u64).ok()
        })
        .collect()
}

fn start(channel: u32, args: &str) -> i32 {
    if STARTED.swap(true, Ordering::AcqRel) {
        return 2;
    }
    let freq = qpc_freq();
    let producer = match Producer::create(channel, &deadlock_schema()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[uw-spike-dlcollector] channel {channel}: {e}");
            return 1;
        }
    };
    let out = Arc::new(Mutex::new(ProducerOut {
        producer,
        throttle: Throttle::new((freq / EVENT_WINDOW_DIV).max(1), EVENT_BURST),
        pending: Sets::new(),
        pending_ts: 0,
    }));
    let handle = OutHandle(out.clone());
    handle.lock().status("initializing", None);

    let mut guard = lock_plugin();
    let plugin = guard.insert(Plugin {
        out: handle,
        mode: Mode::Idle,
    });
    let modules = loaded_modules();

    let discover = DiscoverArgs::parse(args);
    if let Some(seconds) = discover.seconds {
        CHAINS.begin();
        CHAINS.set_window_thread(window_thread());
        let now = qpc_now();
        plugin.mode = Mode::Discovering(Discovering {
            started: now,
            deadline: now.saturating_add(seconds.saturating_mul(freq)),
            done: false,
            out_path: discover.out,
            presents: 0,
        });
        match timing::install(&SINK, None) {
            Ok(()) => plugin.out.lock().status("discovering", None),
            Err(why) => {
                plugin.mode = Mode::Idle;
                plugin
                    .out
                    .lock()
                    .status("error", Some(&format!("QPC hook: {why}")));
            }
        }
        return 0;
    }

    if selftest(args) {
        // The collector resolves nothing in selftest mode, so it never looks at this module.
        let unused = Module {
            name: "client.dll".into(),
            base: 0,
            image_size: 0,
            sections: Vec::new(),
        };
        let collector = Collector::start(
            &SelfProcess,
            &unused,
            Box::new(OutHandle(out.clone())),
            args,
            Box::new(qpc_now),
            freq,
        );
        plugin.mode = Mode::Selftest(Box::new(collector));
        return 0;
    }

    let Some(client) = modules.iter().find(|m| m.name == "client.dll") else {
        plugin
            .out
            .lock()
            .status("error", Some("client.dll not loaded"));
        return 0;
    };
    let mut collector = Collector::start(
        &SelfProcess,
        client,
        Box::new(OutHandle(out.clone())),
        args,
        Box::new(qpc_now),
        freq,
    );
    if matches!(collector.status(), crate::collector::Status::Error(_)) {
        return 0;
    }
    let filter = match resolve_main_loop(&SelfProcess, &modules) {
        Ok(f) => f,
        Err(why) => {
            collector.fail(&why);
            return 0;
        }
    };
    let immediate = filter.immediate;
    plugin.mode = Mode::Collecting {
        collector: Box::new(collector),
        gate: TickGate::new(filter, TickGate::interval_for(freq)),
    };
    // `PLUGIN` stays locked until we return, so no pass runs before the status is published.
    let result = timing::install(&SINK, Some(immediate));
    if let Mode::Collecting { collector, .. } = &mut plugin.mode {
        match result {
            Ok(()) => collector.scanning(),
            Err(why) => collector.fail(&format!("QPC hook: {why}")),
        }
    }
    0
}

/// The module that contains `addr` and the offset into it, if any.
fn locate(addr: u64) -> Frame {
    let mut h = HMODULE::default();
    // SAFETY: FROM_ADDRESS takes the address as the name argument; no reference is taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr as usize as *const u16),
            &mut h,
        )
    };
    if addr == 0 || found.is_err() || h.is_invalid() {
        return Frame {
            module: None,
            rva: addr,
        };
    }
    let mut buf = [0u16; MAX_PATH as usize];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { GetModuleFileNameW(h, &mut buf) } as usize;
    let path = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&path)
        .to_ascii_lowercase();
    Frame {
        module: Some(name),
        rva: addr.wrapping_sub(h.0 as u64),
    }
}

unsafe extern "system" fn window_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut pid = 0u32;
    // SAFETY: `lparam` points at the caller's `u32`; `hwnd` comes from EnumWindows.
    unsafe {
        let thread = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() && IsWindowVisible(hwnd).as_bool() {
            *(lparam.0 as *mut u32) = thread;
            return BOOL(0);
        }
    }
    BOOL(1)
}

/// The thread that owns the game's visible top-level window (0 if none).
fn window_thread() -> u32 {
    let mut thread = 0u32;
    // SAFETY: the callback writes only through the pointer passed here.
    unsafe {
        let _ = EnumWindows(Some(window_cb), LPARAM(&mut thread as *mut u32 as isize));
    }
    thread
}

fn finish_discovery(out: &OutHandle, d: &mut Discovering) {
    timing::stop_discovery();
    d.done = true;
    let Some((chains, busy)) = CHAINS.finish() else {
        return;
    };
    let seconds = (qpc_now().saturating_sub(d.started) as f64 / qpc_freq() as f64).max(1e-3);
    let all = chains_from(&chains, usize::MAX, seconds, locate);
    let top = &all[..all.len().min(DISCOVERY_TOP)];
    let json = discovery_json(top, busy, chains.dropped(), DISCOVERY_MAX as usize);
    let file_error = d.out_path.as_deref().and_then(|path| {
        let totals = DiscoveryTotals {
            seconds,
            window_thread_id: CHAINS.window_thread(),
            dropped_busy: busy,
            dropped_full: chains.dropped(),
        };
        let r = std::fs::write(path, discovery_file_json(&all, &totals));
        eprintln!(
            "[uw-spike-dlcollector] discovery table ({} chains) -> {path}: {r:?}",
            all.len()
        );
        r.err().map(|e| format!("discover_out {path}: {e}"))
    });
    let mut out = out.lock();
    out.commit(
        &[(
            GEP_INTERNAL.into(),
            "discovery".into(),
            Some(json.into_bytes()),
        )],
        ms_now(),
    );
    out.status("discovered", file_error.as_deref());
}

fn present() {
    let mut guard = lock_plugin();
    let Some(Plugin { out, mode }) = guard.as_mut() else {
        return;
    };
    match mode {
        Mode::Collecting { collector, .. } => collector.check_stall(),
        Mode::Discovering(d) if !d.done && qpc_now() >= d.deadline => finish_discovery(out, d),
        Mode::Discovering(d) if !d.done => {
            // The window may appear (or be recreated) after the start.
            d.presents = d.presents.wrapping_add(1);
            if CHAINS.window_thread() == 0 || d.presents % WINDOW_REFRESH_PRESENTS == 0 {
                CHAINS.set_window_thread(window_thread());
            }
        }
        // Retries the selftest event if the throttle or a busy channel refused it.
        Mode::Selftest(collector) => collector.tick(&SelfProcess),
        Mode::Discovering(_) | Mode::Idle => {}
    }
    out.lock().flush();
}

/// Starts the collector on data channel `channel` with the space-separated `args`
/// (`args_len` bytes of UTF-8 at `args`). 0 = started (its status is on the channel); 1 = the
/// channel could not be created; 2 = already started; 3 = start panicked.
///
/// # Safety
/// `args` must point at `args_len` readable bytes (or be null with `args_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uw_plugin_start(channel: u32, args: *const u8, args_len: usize) -> i32 {
    let args = if args.is_null() || args_len == 0 {
        String::new()
    } else {
        // SAFETY: the caller guarantees `args_len` readable bytes.
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(args, args_len) }).into_owned()
    };
    catch_unwind(AssertUnwindSafe(|| start(channel, &args))).unwrap_or(3)
}

#[unsafe(no_mangle)]
pub extern "C" fn uw_plugin_present() {
    let _ = catch_unwind(present);
}

/// Non-zero: the plugin refuses to be unloaded (its hook and channel stay live).
#[unsafe(no_mangle)]
pub extern "C" fn uw_plugin_stop() -> i32 {
    1
}
