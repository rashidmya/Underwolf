//! The helper's portable decision logic: wait for the game and its UI thread, take the helper
//! lock, create the control channel (doorbells, mask region, state block, overlay pipe) and then
//! the bootstrap pipe, skip the hook when the marker mutex already exists (the pinned bootstrap
//! will reconnect), otherwise set `WH_GETMESSAGE` (fall back to `WH_CALLWNDPROC` on a set failure or a hello
//! timeout), post wake-ups and re-wake every `REWAKE_MS` while waiting, and classify the `hello`.
//! The game exiting during any wait ends the run with `GameExited`. Side effects go through the
//! `Win32` trait so this is unit-tested on Linux with a fake; the real implementation lives in the
//! `cfg(windows)` bin.

use uw_pipe_proto::jsonl::Event;
use uw_pipe_proto::pipe::{decode, Message, ProtoError, PROTOCOL_VERSION};

/// While waiting for `hello` with a hook set, the UI thread is woken again this often (design
/// failure table: "no `hello` within 10 s despite wake-ups"). An idle thread that only pumps sent
/// messages (`WH_CALLWNDPROC`) may need several nudges before its hook runs.
pub const REWAKE_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    GetMessage,
    CallWndProc,
}

fn other(kind: HookKind) -> HookKind {
    match kind {
        HookKind::GetMessage => HookKind::CallWndProc,
        HookKind::CallWndProc => HookKind::GetMessage,
    }
}

/// The first message read from the pipe after a hook is set (design step 6/7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloWait {
    Hello { protocol_version: u16, overlay_loaded: bool, overlay_path: Option<String> },
    Timeout,
    PipeError(String),
}

/// Why `Win32::create_channel` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// A state block the game held across a helper restart is not layout v1 (design 2a:
    /// `version_mismatch`, "restart the game").
    VersionMismatch,
    /// An object or the overlay pipe could not be created.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    AlreadyRunning,
    NoWindow,
    HookFailed(u32),
    BootstrapTimeout,
    VersionMismatch,
    /// The game process ended before the bootstrap connected (design failure table, "Game exits").
    GameExited,
    Connected { overlay_loaded: bool, overlay_path: Option<String> },
    PipeError(String),
    /// The control channel's objects or the overlay pipe could not be created.
    ChannelFailed(String),
}

impl Outcome {
    /// The JSON-lines event the helper emits for this outcome.
    pub fn to_event(&self) -> Event {
        match self {
            Outcome::Connected { overlay_loaded, overlay_path } => {
                Event::BootstrapConnected { overlay_loaded: *overlay_loaded, overlay_path: overlay_path.clone() }
            }
            // `hook_failed` keeps its GetLastError as the event's `detail` (design failure table).
            Outcome::HookFailed(e) => Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(*e) },
            Outcome::AlreadyRunning => Event::error("already_running"),
            Outcome::NoWindow => Event::error("no_window"),
            Outcome::BootstrapTimeout => Event::error("bootstrap_timeout"),
            Outcome::VersionMismatch => Event::error("version_mismatch"),
            Outcome::GameExited => Event::GameExited,
            // `pipe_error` is a helper-local I/O failure not in the design's failure table; it maps
            // here so the host still sees a structured error rather than a silent exit.
            Outcome::PipeError(_) => Event::error("pipe_error"),
            // Like `pipe_error`: a helper-local failure, reported so the host sees a structured
            // error rather than a silent exit.
            Outcome::ChannelFailed(_) => Event::error("channel_failed"),
        }
    }
}

pub struct Config {
    pub window_timeout_ms: u64,
    pub hello_timeout_ms: u64,
    pub window_poll_ms: u64,
    /// Force `WH_CALLWNDPROC` first (a test flag; design "forced `WH_CALLWNDPROC`").
    pub force_fallback: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            window_timeout_ms: 120_000,
            hello_timeout_ms: 10_000,
            window_poll_ms: 500,
            force_fallback: false,
        }
    }
}

/// Side effects the helper performs, abstracted so the logic above is unit-tested with a fake.
/// `<pid>` is the game's Windows (Wine) process id.
pub trait Win32 {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// The game's Windows pid, once Toolhelp finds it.
    fn find_game(&mut self) -> Option<u32>;
    /// The thread owning the game's largest visible top-level window without an owner.
    fn find_ui_thread(&mut self, pid: u32) -> Option<u32>;
    /// Whether the game process is still running (polled between waits).
    fn game_alive(&mut self, pid: u32) -> bool;
    /// Whether `Local\underwolf-bootstrap-<pid>` already exists (a pinned bootstrap is present).
    fn marker_exists(&mut self, pid: u32) -> bool;
    /// Take `Local\underwolf-helper-<pid>`; `false` means another helper already holds it.
    fn take_helper_lock(&mut self, pid: u32) -> bool;
    /// Create the control channel (design 2a flow step 2: the doorbell events, the mask region,
    /// the state block, then the overlay pipe). Called right before `create_pipe`.
    fn create_channel(&mut self, pid: u32) -> Result<(), ChannelError>;
    /// Create the bootstrap pipe server before hooking (design step 4).
    fn create_pipe(&mut self, pid: u32) -> Result<(), String>;
    /// Set a thread-scoped hook; `Err` carries `GetLastError`.
    fn set_hook(&mut self, kind: HookKind, thread: u32) -> Result<(), u32>;
    /// Post a few `WM_NULL` to the UI thread so its next message pump runs the hook. Called right
    /// after each hook is set and again every `REWAKE_MS` while waiting for `hello`.
    fn post_wakeups(&mut self, thread: u32);
    /// Accept the bootstrap and read its first message, blocking at most `timeout_ms`. `run`
    /// calls this in slices of at most `REWAKE_MS`: a `Timeout` must leave the pipe able to accept
    /// the bootstrap on the next call.
    fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait;
    /// Remove the current hook (the pin keeps the bootstrap loaded). A no-op if none is set.
    fn unhook(&mut self);
    /// Emit a diagnostic line (stderr in the real helper; a no-op in the fake).
    fn log(&self, _msg: &str) {}
}

/// Classify the first pipe message exactly as the helper reads it. A frame whose *header* version
/// differs (`BadVersion`) comes from a bootstrap of another protocol version, so it is reported as
/// a hello with that version: `run` then returns `VersionMismatch` (design failure table, "tells
/// the user to restart the game") instead of `PipeError`. Without this, `decode`'s header check
/// would make `version_mismatch` unreachable.
pub fn hello_wait_from_bytes(bytes: &[u8]) -> HelloWait {
    match decode(bytes) {
        Ok(Message::Hello { protocol_version, overlay_loaded, overlay_path, .. }) => HelloWait::Hello {
            protocol_version,
            overlay_loaded,
            overlay_path: (!overlay_path.is_empty()).then_some(overlay_path),
        },
        Err(ProtoError::BadVersion(v)) => {
            HelloWait::Hello { protocol_version: v, overlay_loaded: false, overlay_path: None }
        }
        Ok(_) => HelloWait::PipeError("expected hello".into()),
        Err(e) => HelloWait::PipeError(format!("decode hello: {e}")),
    }
}

/// The channel, then the bootstrap pipe (design 2a flow step 2). `Err` ends the run.
fn create_pipes(w: &mut dyn Win32, pid: u32) -> Result<(), Outcome> {
    match w.create_channel(pid) {
        Ok(()) => {}
        Err(ChannelError::VersionMismatch) => return Err(Outcome::VersionMismatch),
        Err(ChannelError::Failed(e)) => return Err(Outcome::ChannelFailed(e)),
    }
    w.create_pipe(pid).map_err(Outcome::PipeError)
}

fn outcome_from_hello(hw: HelloWait) -> Result<Outcome, ()> {
    match hw {
        HelloWait::Hello { protocol_version, overlay_loaded, overlay_path } => {
            if protocol_version != PROTOCOL_VERSION {
                Ok(Outcome::VersionMismatch)
            } else {
                Ok(Outcome::Connected { overlay_loaded, overlay_path })
            }
        }
        HelloWait::PipeError(e) => Ok(Outcome::PipeError(e)),
        HelloWait::Timeout => Err(()),
    }
}

/// Wait up to `timeout_ms` for the hello in `REWAKE_MS` slices. Between slices, end with
/// `GameExited` if the game has gone, and re-wake `wake` (the hooked UI thread, if any).
/// `Err(())` is a timeout.
fn await_hello(w: &mut dyn Win32, pid: u32, wake: Option<u32>, timeout_ms: u64) -> Result<Outcome, ()> {
    let start = w.now_ms();
    loop {
        let left = timeout_ms.saturating_sub(w.now_ms().saturating_sub(start));
        if let Ok(o) = outcome_from_hello(w.wait_hello(left.min(REWAKE_MS))) {
            return Ok(o);
        }
        if !w.game_alive(pid) {
            return Ok(Outcome::GameExited);
        }
        if w.now_ms().saturating_sub(start) >= timeout_ms {
            return Err(());
        }
        if let Some(ui) = wake {
            w.post_wakeups(ui);
        }
    }
}

/// Drive one helper run to an outcome.
pub fn run(w: &mut dyn Win32, cfg: &Config) -> Outcome {
    let start = w.now_ms();

    // 1. Find the game, waiting for it to appear within the window timeout.
    let pid = loop {
        if let Some(pid) = w.find_game() {
            break pid;
        }
        if w.now_ms().saturating_sub(start) >= cfg.window_timeout_ms {
            return Outcome::NoWindow;
        }
        w.sleep_ms(cfg.window_poll_ms);
    };

    // 2. One helper per game.
    if !w.take_helper_lock(pid) {
        return Outcome::AlreadyRunning;
    }

    // 3. Already there? The pinned bootstrap from an earlier helper will reconnect: create the
    //    channel and the pipe, skip the hook, and wait for its hello (no hook, so nothing to wake).
    if w.marker_exists(pid) {
        w.log("marker present; skipping hook");
        if let Err(o) = create_pipes(w, pid) {
            return o;
        }
        return await_hello(w, pid, None, cfg.hello_timeout_ms).unwrap_or(Outcome::BootstrapTimeout);
    }

    // 4. Hook. Find the UI thread (still within the window timeout, while the game runs).
    let ui = loop {
        if let Some(t) = w.find_ui_thread(pid) {
            break t;
        }
        if !w.game_alive(pid) {
            return Outcome::GameExited;
        }
        if w.now_ms().saturating_sub(start) >= cfg.window_timeout_ms {
            return Outcome::NoWindow;
        }
        w.sleep_ms(cfg.window_poll_ms);
    };
    if let Err(o) = create_pipes(w, pid) {
        return o;
    }

    let first = if cfg.force_fallback { HookKind::CallWndProc } else { HookKind::GetMessage };

    // Set the first hook; a set failure falls back to the other hook immediately.
    let active = match w.set_hook(first, ui) {
        Ok(()) => first,
        Err(e) => match w.set_hook(other(first), ui) {
            Ok(()) => other(first),
            Err(_) => return Outcome::HookFailed(e),
        },
    };
    w.post_wakeups(ui);

    match await_hello(w, pid, Some(ui), cfg.hello_timeout_ms) {
        Ok(o) => {
            w.unhook();
            o
        }
        Err(()) => {
            // No hello: unhook and try the other hook once (design "try the fallback hook once").
            w.unhook();
            if w.set_hook(other(active), ui).is_err() {
                return Outcome::BootstrapTimeout;
            }
            w.post_wakeups(ui);
            let result =
                await_hello(w, pid, Some(ui), cfg.hello_timeout_ms).unwrap_or(Outcome::BootstrapTimeout);
            w.unhook();
            result
        }
    }
}
