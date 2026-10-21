//! Main-thread timing (spec §6.4 "Loading and timing"). The collector hooks
//! `QueryPerformanceCounter` and runs a pass only when the call comes from the game's main-loop
//! call site, at most every `PASS_INTERVAL_MS`.
//!
//! The portable part is the gate ([`TickGate`]) and discovery bookkeeping ([`Discovery`]); the
//! `cfg(windows)` part is the MinHook detour on `ntdll!RtlQueryPerformanceCounter` (Wine's
//! `kernel32!QueryPerformanceCounter` forwards there, so every caller passes through).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};

/// 20 Hz.
pub const PASS_INTERVAL_MS: u64 = 50;
/// Return addresses recorded per QPC call in discovery mode.
pub const DISCOVERY_FRAMES: usize = 5;
/// Threads tracked per chain in discovery mode.
pub const DISCOVERY_THREADS: usize = 4;
/// Chains considered for the `gep_internal.discovery` summary (those that fit 4096 bytes).
pub const DISCOVERY_TOP: usize = 32;

/// Return addresses that identify the main-loop QPC call: `immediate` is the caller's return
/// address; `outer`, when set, must be the next return address up the stack (the game reaches
/// QPC through a tier0 wrapper).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerFilter {
    pub immediate: u64,
    pub outer: Option<u64>,
}

pub struct TickGate {
    filter: CallerFilter,
    interval_ticks: u64,
    last_pass: Option<u64>,
    in_pass: bool,
}

impl TickGate {
    pub fn new(filter: CallerFilter, interval_ticks: u64) -> TickGate {
        TickGate {
            filter,
            interval_ticks,
            last_pass: None,
            in_pass: false,
        }
    }

    /// `PASS_INTERVAL_MS` in ticks of a `freq` Hz counter (at least 1).
    pub fn interval_for(freq: u64) -> u64 {
        u64::try_from(u128::from(freq) * u128::from(PASS_INTERVAL_MS) / 1000)
            .unwrap_or(u64::MAX)
            .max(1)
    }

    pub fn filter(&self) -> CallerFilter {
        self.filter
    }

    /// Whether the QPC call returning to `ret` at counter value `now` starts a pass. `outer` (a
    /// stack walk) runs only for the matching call site, once the interval has elapsed. A `true`
    /// must be followed by `done()` when the pass ends; until then every call returns `false`.
    pub fn should_run(&mut self, ret: u64, now: u64, outer: impl FnOnce() -> Option<u64>) -> bool {
        if ret != self.filter.immediate || self.in_pass {
            return false;
        }
        if self
            .last_pass
            .is_some_and(|last| now.wrapping_sub(last) < self.interval_ticks)
        {
            return false;
        }
        if let Some(want) = self.filter.outer
            && outer() != Some(want)
        {
            return false;
        }
        self.in_pass = true;
        self.last_pass = Some(now);
        true
    }

    pub fn done(&mut self) {
        self.in_pass = false;
    }
}

/// Per-chain counts in discovery mode. A thread slot with id 0 is empty (no thread has id 0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChainStats {
    pub calls: u64,
    /// Calls made on the thread that owns the game window.
    pub window_calls: u64,
    /// The first `DISCOVERY_THREADS` threads seen, with their call counts.
    pub threads: [(u32, u64); DISCOVERY_THREADS],
    /// Calls from further threads.
    pub other_thread_calls: u64,
}

impl ChainStats {
    fn add(&mut self, thread: u32, on_window: bool) {
        self.calls += 1;
        self.window_calls += u64::from(on_window);
        for slot in &mut self.threads {
            if slot.0 == thread {
                slot.1 += 1;
                return;
            }
            if slot.0 == 0 {
                *slot = (thread, 1);
                return;
            }
        }
        self.other_thread_calls += 1;
    }
}

/// QPC call chains counted in discovery mode. The map is sized for `MAX_CHAINS` up front and the
/// per-chain stats are fixed-size, so `record` never allocates.
pub struct Discovery {
    counts: HashMap<[u64; DISCOVERY_FRAMES], ChainStats>,
    dropped: u64,
}

impl Discovery {
    /// Distinct chains kept; calls of further new chains are only counted as dropped.
    pub const MAX_CHAINS: usize = 4096;

    pub fn new() -> Discovery {
        Discovery {
            counts: HashMap::with_capacity(Discovery::MAX_CHAINS),
            dropped: 0,
        }
    }

    /// Entries the map holds without reallocating.
    pub fn capacity(&self) -> usize {
        self.counts.capacity()
    }

    pub fn record(&mut self, frames: [u64; DISCOVERY_FRAMES], thread: u32, on_window: bool) {
        let full = self.counts.len() >= Discovery::MAX_CHAINS;
        match self.counts.get_mut(&frames) {
            Some(s) => s.add(thread, on_window),
            None if full => self.dropped += 1,
            None => {
                let mut s = ChainStats::default();
                s.add(thread, on_window);
                self.counts.insert(frames, s);
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// The `n` most called chains, most calls first; ties by chain.
    pub fn top(&self, n: usize) -> Vec<([u64; DISCOVERY_FRAMES], ChainStats)> {
        let mut all: Vec<_> = self.counts.iter().map(|(&f, &s)| (f, s)).collect();
        all.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(&b.0)));
        all.truncate(n);
        all
    }
}

impl Default for Discovery {
    fn default() -> Discovery {
        Discovery::new()
    }
}

/// The discovery state shared by every game thread. A recording thread only tries the lock: when
/// another thread (or the present thread) holds it, the call is counted in `busy` and dropped, so
/// game threads never wait on each other.
pub struct SharedDiscovery {
    chains: Mutex<Option<Discovery>>,
    busy: AtomicU64,
    window_thread: AtomicU32,
}

impl SharedDiscovery {
    pub const fn new() -> SharedDiscovery {
        SharedDiscovery {
            chains: Mutex::new(None),
            busy: AtomicU64::new(0),
            window_thread: AtomicU32::new(0),
        }
    }

    /// Starts a fresh recording.
    pub fn begin(&self) {
        *self.lock() = Some(Discovery::new());
        self.busy.store(0, Ordering::Relaxed);
    }

    /// The thread that owns the game window (0: unknown). Calls on it count as `window_calls`.
    pub fn set_window_thread(&self, thread: u32) {
        self.window_thread.store(thread, Ordering::Relaxed);
    }

    pub fn window_thread(&self) -> u32 {
        self.window_thread.load(Ordering::Relaxed)
    }

    /// Counts one call made on `thread`; `false` if it was not recorded (busy, or not recording).
    pub fn record(&self, frames: [u64; DISCOVERY_FRAMES], thread: u32) -> bool {
        let window = self.window_thread.load(Ordering::Relaxed);
        let on_window = window != 0 && thread == window;
        let mut guard = match self.chains.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.busy.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        };
        match guard.as_mut() {
            Some(d) => {
                d.record(frames, thread, on_window);
                true
            }
            None => false,
        }
    }

    /// Ends the recording: the chains and the number of calls dropped because the lock was busy.
    pub fn finish(&self) -> Option<(Discovery, u64)> {
        let d = self.lock().take()?;
        Some((d, self.busy.load(Ordering::Relaxed)))
    }

    /// Blocking access, for the present thread only (never from a QPC call).
    pub fn lock(&self) -> MutexGuard<'_, Option<Discovery>> {
        self.chains.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Default for SharedDiscovery {
    fn default() -> SharedDiscovery {
        SharedDiscovery::new()
    }
}

/// `discover=<seconds>` (1..=600) and `discover_out=<windows path>` from the start args. Paths
/// cannot contain spaces (args are space-separated).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoverArgs {
    pub seconds: Option<u64>,
    pub out: Option<String>,
}

impl DiscoverArgs {
    pub const MAX_SECONDS: u64 = 600;

    pub fn parse(args: &str) -> DiscoverArgs {
        let mut a = DiscoverArgs::default();
        for (key, value) in args.split_whitespace().filter_map(|t| t.split_once('=')) {
            match key {
                "discover" => {
                    a.seconds = value
                        .parse::<u64>()
                        .ok()
                        .filter(|s| (1..=DiscoverArgs::MAX_SECONDS).contains(s))
                }
                "discover_out" if !value.is_empty() => a.out = Some(value.to_string()),
                _ => {}
            }
        }
        a
    }
}

/// One return address: the module containing it and the offset from its base, or no module and
/// the absolute address.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub module: Option<String>,
    pub rva: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub frames: Vec<Frame>,
    pub calls: u64,
    pub per_sec: f64,
    /// Some calls came from the thread that owns the game window.
    pub window_thread: bool,
    pub window_calls: u64,
    /// Thread id and calls, most calls first.
    pub threads: Vec<(u32, u64)>,
    pub other_thread_calls: u64,
}

/// The `n` most called chains as output records. `locate` names a return address; the walk's
/// trailing zero frames are left out.
pub fn chains_from(
    d: &Discovery,
    n: usize,
    seconds: f64,
    locate: impl Fn(u64) -> Frame,
) -> Vec<Chain> {
    let seconds = if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        1.0
    };
    d.top(n)
        .into_iter()
        .map(|(frames, s)| {
            let used = frames.iter().rposition(|&f| f != 0).map_or(0, |i| i + 1);
            let mut threads: Vec<(u32, u64)> =
                s.threads.iter().copied().filter(|t| t.0 != 0).collect();
            threads.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            Chain {
                frames: frames[..used].iter().map(|&a| locate(a)).collect(),
                calls: s.calls,
                per_sec: s.calls as f64 / seconds,
                window_thread: s.window_calls > 0,
                window_calls: s.window_calls,
                threads,
                other_thread_calls: s.other_thread_calls,
            }
        })
        .collect()
}

/// Totals for the discovery file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiscoveryTotals {
    pub seconds: f64,
    /// 0: the game window was not found.
    pub window_thread_id: u32,
    pub dropped_busy: u64,
    pub dropped_full: u64,
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn num(v: f64) -> f64 {
    if v.is_finite() {
        (v * 10.0).round() / 10.0
    } else {
        0.0
    }
}

fn frames_json(c: &Chain) -> String {
    let frames: Vec<String> = c
        .frames
        .iter()
        .map(|f| {
            let module = f
                .module
                .as_deref()
                .map_or_else(|| "null".to_string(), json_string);
            format!("{{\"module\":{module},\"rva\":\"{:#x}\"}}", f.rva)
        })
        .collect();
    format!("[{}]", frames.join(","))
}

fn chain_json(c: &Chain) -> String {
    format!(
        "{{\"frames\":{},\"per_sec\":{},\"window_thread\":{}}}",
        frames_json(c),
        num(c.per_sec),
        c.window_thread
    )
}

fn chain_full_json(c: &Chain) -> String {
    let threads: Vec<String> = c
        .threads
        .iter()
        .map(|(tid, calls)| format!("{{\"tid\":{tid},\"calls\":{calls}}}"))
        .collect();
    format!(
        "{{\"frames\":{},\"calls\":{},\"per_sec\":{},\"window_thread\":{},\"window_calls\":{},\"threads\":[{}],\"other_thread_calls\":{}}}",
        frames_json(c),
        c.calls,
        num(c.per_sec),
        c.window_thread,
        c.window_calls,
        threads.join(","),
        c.other_thread_calls
    )
}

/// The summary for `gep_internal.discovery`:
/// `{dropped_busy, dropped_full, chains:[{frames:[{module,rva}], per_sec, window_thread}]}`,
/// keeping the leading chains that fit in `max` bytes. `dropped_busy` counts calls skipped because
/// the recording lock was held, `dropped_full` calls of new chains after `MAX_CHAINS`. RVAs are hex
/// strings (a u64 does not fit a JSON number exactly).
pub fn discovery_json(
    chains: &[Chain],
    dropped_busy: u64,
    dropped_full: u64,
    max: usize,
) -> String {
    let mut out =
        format!("{{\"dropped_busy\":{dropped_busy},\"dropped_full\":{dropped_full},\"chains\":[");
    let empty = out.len();
    for c in chains {
        let item = chain_json(c);
        let sep = usize::from(out.len() > empty);
        if out.len() + sep + item.len() + 2 > max {
            break;
        }
        if sep == 1 {
            out.push(',');
        }
        out.push_str(&item);
    }
    out.push_str("]}");
    out
}

/// The full chain table for the `discover_out` file, one chain per line:
/// `{seconds, window_thread_id, dropped_busy, dropped_full, chains:[{frames, calls, per_sec,
/// window_thread, window_calls, threads:[{tid,calls}], other_thread_calls}]}`.
pub fn discovery_file_json(chains: &[Chain], t: &DiscoveryTotals) -> String {
    let items: Vec<String> = chains.iter().map(chain_full_json).collect();
    format!(
        "{{\"seconds\":{},\"window_thread_id\":{},\"dropped_busy\":{},\"dropped_full\":{},\"chains\":[\n{}\n]}}\n",
        num(t.seconds),
        t.window_thread_id,
        t.dropped_busy,
        t.dropped_full,
        items.join(",\n")
    )
}

#[cfg(windows)]
pub use hook::*;

#[cfg(windows)]
mod hook {
    //! The QPC detour. The hot path (a call from anywhere but the main-loop site) is one atomic
    //! load, a compare and a tail call into the trampoline. Discovery mode installs a separate
    //! detour that records every call.

    use std::cell::Cell;
    use std::ffi::c_void;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{s, w};

    use super::DISCOVERY_FRAMES;

    type QpcFn = unsafe extern "system" fn(*mut i64) -> i32;

    /// What the handler does on a matching call or (in discovery mode) on every call.
    pub trait QpcSink: Sync {
        /// A QPC call returned to `ret` (the main-loop site); `outer` walks one more frame.
        fn main_loop_call(&self, ret: u64, outer: &dyn Fn() -> Option<u64>);
        /// Discovery mode: every QPC call, with its first return addresses.
        fn discovery_call(&self, frames: [u64; DISCOVERY_FRAMES]);
    }

    static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
    /// The main-loop return address; 0 matches nothing (no return address is 0).
    static IMMEDIATE: AtomicU64 = AtomicU64::new(0);
    static DISCOVERING: AtomicBool = AtomicBool::new(false);
    static SINK: OnceLock<&'static dyn QpcSink> = OnceLock::new();

    thread_local! {
        /// Set while this thread is inside our own code (a pass or a discovery record), so QPC
        /// calls made from there are passed straight through.
        static BUSY: Cell<bool> = const { Cell::new(false) };
    }

    // The detours pass the caller's return address to the handler without changing the stack, so
    // the handler returns straight to the caller with the real function's `eax`.
    #[unsafe(naked)]
    unsafe extern "system" fn qpc_detour() {
        core::arch::naked_asm!("mov rdx, [rsp]", "jmp {h}", h = sym qpc_handler)
    }

    #[unsafe(naked)]
    unsafe extern "system" fn qpc_discovery_detour() {
        core::arch::naked_asm!("mov rdx, [rsp]", "jmp {h}", h = sym qpc_discovery_handler)
    }

    #[inline(always)]
    fn real() -> QpcFn {
        // SAFETY: set to MinHook's trampoline before the hook is enabled, never changed after.
        unsafe { std::mem::transmute::<usize, QpcFn>(TRAMPOLINE.load(Ordering::Relaxed)) }
    }

    unsafe extern "system" fn qpc_handler(counter: *mut i64, ret: usize) -> i32 {
        if ret as u64 != IMMEDIATE.load(Ordering::Relaxed) {
            return unsafe { real()(counter) };
        }
        let r = unsafe { real()(counter) };
        guarded(|sink| sink.main_loop_call(ret as u64, &|| frame_after(ret as u64)));
        r
    }

    unsafe extern "system" fn qpc_discovery_handler(counter: *mut i64, ret: usize) -> i32 {
        let r = unsafe { real()(counter) };
        if DISCOVERING.load(Ordering::Relaxed) {
            guarded(|sink| sink.discovery_call(chain_from(ret as u64)));
        }
        r
    }

    /// Runs `f` unless this thread is already inside our code; a panic never unwinds into the
    /// game's frames.
    #[inline(never)]
    fn guarded(f: impl FnOnce(&'static dyn QpcSink)) {
        let Some(&sink) = SINK.get() else { return };
        if BUSY.with(|b| b.replace(true)) {
            return;
        }
        let _ = catch_unwind(AssertUnwindSafe(|| f(sink)));
        BUSY.with(|b| b.set(false));
    }

    fn backtrace() -> ([*mut c_void; 32], usize) {
        let mut frames = [std::ptr::null_mut(); 32];
        // SAFETY: the buffer holds `frames.len()` entries.
        let n = unsafe { RtlCaptureStackBackTrace(0, &mut frames, None) } as usize;
        (frames, n.min(32))
    }

    /// The return address one frame above `ret` on this thread's stack.
    fn frame_after(ret: u64) -> Option<u64> {
        let (frames, n) = backtrace();
        let at = frames[..n].iter().position(|&f| f as u64 == ret)?;
        frames[..n].get(at + 1).map(|&f| f as u64)
    }

    /// `ret` and the two return addresses above it (0 where the walk ends).
    fn chain_from(ret: u64) -> [u64; DISCOVERY_FRAMES] {
        let mut out = [0u64; DISCOVERY_FRAMES];
        out[0] = ret;
        let (frames, n) = backtrace();
        if let Some(at) = frames[..n].iter().position(|&f| f as u64 == ret) {
            for (slot, f) in out[1..].iter_mut().zip(&frames[at + 1..n]) {
                *slot = *f as u64;
            }
        }
        out
    }

    /// Hooks `ntdll!RtlQueryPerformanceCounter` once. `immediate` is the main-loop return address;
    /// `None` is discovery mode, where every call goes to `sink.discovery_call` until
    /// [`stop_discovery`].
    pub fn install(sink: &'static dyn QpcSink, immediate: Option<u64>) -> Result<(), String> {
        if SINK.set(sink).is_err() {
            return Err("QPC hook already installed".into());
        }
        let target = unsafe {
            let ntdll = GetModuleHandleW(w!("ntdll.dll")).map_err(|e| format!("ntdll.dll: {e}"))?;
            GetProcAddress(ntdll, s!("RtlQueryPerformanceCounter"))
                .ok_or("RtlQueryPerformanceCounter not found")?
        };
        let detour = match immediate {
            Some(addr) => {
                IMMEDIATE.store(addr, Ordering::Relaxed);
                qpc_detour as *mut c_void
            }
            None => {
                DISCOVERING.store(true, Ordering::Relaxed);
                qpc_discovery_detour as *mut c_void
            }
        };
        // SAFETY: `target` is the exported function; the detour takes its argument unchanged
        // (plus the return address the stub adds) and returns its result.
        let trampoline = unsafe { minhook::MinHook::create_hook(target as *mut c_void, detour) }
            .map_err(|e| format!("MinHook create_hook: {e:?}"))?;
        TRAMPOLINE.store(trampoline as usize, Ordering::Release);
        unsafe { minhook::MinHook::enable_hook(target as *mut c_void) }
            .map_err(|e| format!("MinHook enable_hook: {e:?}"))?;
        Ok(())
    }

    /// Ends discovery: later calls only pass through.
    pub fn stop_discovery() {
        DISCOVERING.store(false, Ordering::Relaxed);
    }
}
