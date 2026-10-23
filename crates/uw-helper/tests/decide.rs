use std::collections::VecDeque;

use uw_helper::decide::{
    hello_wait_from_bytes, run, Config, HelloWait, HookKind, Outcome, Win32, REWAKE_MS,
};
use uw_pipe_proto::jsonl::Event;
use uw_pipe_proto::pipe::{encode, Message, StatusCode, PROTOCOL_VERSION};

/// A wait that never ends would spin the fake clock forever; past this the fake panics, so a
/// removed timeout branch fails its test instead of hanging it.
const FAKE_CLOCK_LIMIT_MS: u64 = 3_600_000;

struct Fake {
    now: u64,
    game: Option<u32>,
    ui: Option<u32>,
    /// The UI thread's window only exists from this fake time on.
    ui_from_ms: u64,
    /// The game process ends at this fake time (`None`: it keeps running).
    alive_until: Option<u64>,
    marker: bool,
    lock_free: bool,
    pipe: Result<(), String>,
    getmessage_ok: bool,
    callwndproc_ok: bool,
    /// One entry per hook attempt (or per marker-path wait). A `Timeout` entry (or an empty queue)
    /// makes every `wait_hello` slice time out, advancing the fake clock by the slice, until
    /// `unhook` retires it; any other entry is returned by the next `wait_hello`.
    hellos: VecDeque<HelloWait>,
    /// The hook only runs (so a queued hello is only delivered) once this many wake-ups have been
    /// posted since the hook was set, like an idle UI thread that needs a nudge.
    wakes_before_hello: usize,
    wakes_since_hook: usize,
    /// `post_wakeups` calls per hook attempt, in order.
    wakes_per_hook: Vec<usize>,
    /// The fake time of each `unhook`.
    unhook_at: Vec<u64>,
    log: Vec<String>,
}

impl Fake {
    fn happy() -> Fake {
        Fake {
            now: 0,
            game: Some(1000),
            ui: Some(2000),
            ui_from_ms: 0,
            alive_until: None,
            marker: false,
            lock_free: true,
            pipe: Ok(()),
            getmessage_ok: true,
            callwndproc_ok: true,
            hellos: VecDeque::new(),
            wakes_before_hello: 0,
            wakes_since_hook: 0,
            wakes_per_hook: Vec::new(),
            unhook_at: Vec::new(),
            log: Vec::new(),
        }
    }

    fn advance(&mut self, ms: u64) {
        self.now += ms;
        assert!(self.now <= FAKE_CLOCK_LIMIT_MS, "fake clock passed an hour: a wait never ended");
    }
}

impl Win32 for Fake {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.advance(ms);
    }
    fn find_game(&mut self) -> Option<u32> {
        self.log.push("find_game".into());
        self.game
    }
    fn find_ui_thread(&mut self, _pid: u32) -> Option<u32> {
        self.log.push("find_ui_thread".into());
        self.ui.filter(|_| self.now >= self.ui_from_ms)
    }
    fn game_alive(&mut self, _pid: u32) -> bool {
        self.alive_until.is_none_or(|t| self.now < t)
    }
    fn marker_exists(&mut self, _pid: u32) -> bool {
        self.marker
    }
    fn take_helper_lock(&mut self, _pid: u32) -> bool {
        self.lock_free
    }
    fn create_pipe(&mut self, _pid: u32) -> Result<(), String> {
        self.log.push("create_pipe".into());
        self.pipe.clone()
    }
    fn set_hook(&mut self, kind: HookKind, _thread: u32) -> Result<(), u32> {
        self.log.push(format!("set_hook {kind:?}"));
        let ok = match kind {
            HookKind::GetMessage => self.getmessage_ok,
            HookKind::CallWndProc => self.callwndproc_ok,
        };
        if ok {
            self.wakes_since_hook = 0;
            self.wakes_per_hook.push(0);
            Ok(())
        } else {
            Err(1)
        }
    }
    fn post_wakeups(&mut self, _thread: u32) {
        self.log.push("post_wakeups".into());
        self.wakes_since_hook += 1;
        if let Some(n) = self.wakes_per_hook.last_mut() {
            *n += 1;
        }
    }
    fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait {
        self.log.push("wait_hello".into());
        let hook_ran = self.wakes_since_hook >= self.wakes_before_hello;
        match self.hellos.front() {
            Some(h) if *h != HelloWait::Timeout && hook_ran => self.hellos.pop_front().unwrap(),
            _ => {
                self.advance(timeout_ms);
                HelloWait::Timeout
            }
        }
    }
    fn unhook(&mut self) {
        self.log.push("unhook".into());
        self.unhook_at.push(self.now);
        if self.hellos.front() == Some(&HelloWait::Timeout) {
            self.hellos.pop_front();
        }
    }
}

fn cfg() -> Config {
    Config { window_timeout_ms: 20, hello_timeout_ms: 1000, window_poll_ms: 5, force_fallback: false }
}

fn hello(version: u16, overlay_loaded: bool, path: Option<&str>) -> HelloWait {
    HelloWait::Hello { protocol_version: version, overlay_loaded, overlay_path: path.map(Into::into) }
}

#[test]
fn happy_path_hooks_get_message_and_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    let out = run(&mut f, &cfg());
    assert_eq!(out, Outcome::Connected { overlay_loaded: false, overlay_path: None });
    assert!(f.log.contains(&"set_hook GetMessage".to_string()));
    assert!(!f.log.contains(&"set_hook CallWndProc".to_string()));
    assert_eq!(f.log.iter().filter(|c| *c == "unhook").count(), 1);
}

#[test]
fn second_helper_exits_already_running() {
    let mut f = Fake::happy();
    f.lock_free = false;
    assert_eq!(run(&mut f, &cfg()), Outcome::AlreadyRunning);
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
}

#[test]
fn no_game_within_the_window_timeout_is_no_window() {
    let mut f = Fake::happy();
    f.game = None;
    assert_eq!(run(&mut f, &cfg()), Outcome::NoWindow);
}

#[test]
fn game_without_a_window_is_no_window() {
    let mut f = Fake::happy();
    f.ui = None;
    assert_eq!(run(&mut f, &cfg()), Outcome::NoWindow);
}

#[test]
fn set_hook_failure_falls_back_to_callwndproc() {
    let mut f = Fake::happy();
    f.getmessage_ok = false;
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    let i = f.log.iter().position(|c| c == "set_hook GetMessage").unwrap();
    let j = f.log.iter().position(|c| c == "set_hook CallWndProc").unwrap();
    assert!(i < j, "GetMessage is tried before the CallWndProc fallback");
}

#[test]
fn both_hooks_failing_is_hook_failed() {
    let mut f = Fake::happy();
    f.getmessage_ok = false;
    f.callwndproc_ok = false;
    assert_eq!(run(&mut f, &cfg()), Outcome::HookFailed(1));
}

#[test]
fn timeout_then_fallback_hook_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(HelloWait::Timeout); // first hook: no hello
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None)); // fallback hook: hello
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    let i = f.log.iter().position(|c| c == "set_hook GetMessage").unwrap();
    let j = f.log.iter().position(|c| c == "set_hook CallWndProc").unwrap();
    assert!(i < j);
    assert_eq!(f.log.iter().filter(|c| *c == "unhook").count(), 2);
}

#[test]
fn timeout_on_both_hooks_is_bootstrap_timeout() {
    let mut f = Fake::happy();
    // Both wait_hello calls time out (queue empty -> Timeout).
    assert_eq!(run(&mut f, &cfg()), Outcome::BootstrapTimeout);
}

#[test]
fn mismatched_protocol_version_is_version_mismatch() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION + 1, false, None));
    assert_eq!(run(&mut f, &cfg()), Outcome::VersionMismatch);
}

#[test]
fn marker_present_skips_the_hook_and_takes_hello() {
    let mut f = Fake::happy();
    f.marker = true;
    f.hellos.push_back(hello(PROTOCOL_VERSION, true, Some(r"Z:\g\overlay.dll")));
    let out = run(&mut f, &cfg());
    assert_eq!(
        out,
        Outcome::Connected { overlay_loaded: true, overlay_path: Some(r"Z:\g\overlay.dll".into()) }
    );
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
    assert!(!f.log.iter().any(|c| c == "unhook"));
    assert!(f.log.contains(&"create_pipe".to_string()));
}

#[test]
fn force_fallback_sets_callwndproc_first() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    let mut c = cfg();
    c.force_fallback = true;
    assert_eq!(run(&mut f, &c), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    assert_eq!(f.log.iter().find(|c| c.starts_with("set_hook")).unwrap(), "set_hook CallWndProc");
}

#[test]
fn pipe_creation_failure_is_reported() {
    let mut f = Fake::happy();
    f.pipe = Err("busy".into());
    assert_eq!(run(&mut f, &cfg()), Outcome::PipeError("busy".into()));
}

/// A real hello frame, with the payload's protocol version and the header version set apart.
fn hello_bytes(protocol_version: u16, header_version: u16) -> Vec<u8> {
    let mut b = encode(&Message::Hello {
        protocol_version,
        bootstrap_version: 1,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
    b[6..8].copy_from_slice(&header_version.to_le_bytes()); // header `version` field
    b
}

#[test]
fn a_hello_from_another_protocol_version_is_version_mismatch() {
    // A newer bootstrap writes its own version into the header too; decode rejects the header,
    // and the helper must still report version_mismatch, not pipe_error.
    let mut f = Fake::happy();
    f.hellos.push_back(hello_wait_from_bytes(&hello_bytes(PROTOCOL_VERSION + 1, PROTOCOL_VERSION + 1)));
    assert_eq!(run(&mut f, &cfg()), Outcome::VersionMismatch);
}

#[test]
fn a_current_hello_frame_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello_wait_from_bytes(&hello_bytes(PROTOCOL_VERSION, PROTOCOL_VERSION)));
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
}

#[test]
fn a_non_hello_first_message_is_a_pipe_error() {
    let b = encode(&Message::Status { request_id: 1, code: StatusCode::Ok, detail: 0, message: String::new() });
    assert!(matches!(hello_wait_from_bytes(&b), HelloWait::PipeError(_)));
}

#[test]
fn outcomes_map_to_json_events() {
    assert_eq!(
        Outcome::Connected { overlay_loaded: false, overlay_path: None }.to_event(),
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }
    );
    // The pinned overlay's path reaches the host (plan 1 final review M3).
    assert_eq!(
        Outcome::Connected { overlay_loaded: true, overlay_path: Some(r"Z:\g\o.dll".into()) }.to_event(),
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\g\o.dll".into()) }
    );
    assert_eq!(Outcome::NoWindow.to_event(), Event::error("no_window"));
    assert_eq!(Outcome::AlreadyRunning.to_event(), Event::error("already_running"));
    assert_eq!(
        Outcome::HookFailed(5).to_event(),
        Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(5) }
    );
    assert_eq!(Outcome::BootstrapTimeout.to_event(), Event::error("bootstrap_timeout"));
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
    assert_eq!(Outcome::PipeError("x".into()).to_event(), Event::error("pipe_error"));
}

// --- The design's failure table with the production timeouts, on the fake clock. ---

#[test]
fn no_game_is_no_window_after_120_s_by_default() {
    let mut f = Fake::happy();
    f.game = None;
    let c = Config::default();
    assert_eq!(run(&mut f, &c), Outcome::NoWindow);
    assert!(
        (120_000..120_000 + c.window_poll_ms).contains(&f.now),
        "gave up at {} ms, not at 120 s",
        f.now
    );
}

#[test]
fn a_game_without_a_window_is_no_window_after_120_s_by_default() {
    let mut f = Fake::happy();
    f.ui = None;
    let c = Config::default();
    assert_eq!(run(&mut f, &c), Outcome::NoWindow);
    assert!((120_000..120_000 + c.window_poll_ms).contains(&f.now), "gave up at {} ms", f.now);
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
}

#[test]
fn a_window_that_appears_just_before_120_s_is_hooked() {
    let mut f = Fake::happy();
    f.ui_from_ms = 119_500;
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    assert_eq!(
        run(&mut f, &Config::default()),
        Outcome::Connected { overlay_loaded: false, overlay_path: None }
    );
}

#[test]
fn no_hello_is_bootstrap_timeout_after_10_s_per_hook_with_re_wakes() {
    let mut f = Fake::happy();
    let c = Config::default();
    assert_eq!(run(&mut f, &c), Outcome::BootstrapTimeout);
    // Each hook waits the full 10 s, then is removed; the fallback is tried exactly once.
    assert_eq!(f.unhook_at, vec![10_000, 20_000]);
    let hooks: Vec<_> = f.log.iter().filter(|c| c.starts_with("set_hook")).collect();
    assert_eq!(hooks, ["set_hook GetMessage", "set_hook CallWndProc"]);
    // The UI thread is woken right after each hook is set and again every REWAKE_MS until the
    // timeout, not just once ("no hello within 10 s despite wake-ups").
    let per_hook = (10_000 / REWAKE_MS) as usize;
    assert!(per_hook > 1);
    assert_eq!(f.wakes_per_hook, vec![per_hook, per_hook]);
}

#[test]
fn a_hook_that_runs_only_on_a_later_wake_up_connects_without_the_fallback() {
    // An idle UI thread runs the hook only on the fourth wake-up: the re-wakes, not the fallback,
    // must bring the hello in.
    let mut f = Fake::happy();
    f.wakes_before_hello = 4;
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    assert_eq!(
        run(&mut f, &Config::default()),
        Outcome::Connected { overlay_loaded: false, overlay_path: None }
    );
    assert!(!f.log.contains(&"set_hook CallWndProc".to_string()));
    assert!(f.now < 10_000);
}

#[test]
fn a_pinned_bootstrap_of_another_version_is_version_mismatch() {
    // The reconnect path: a bootstrap pinned by an older helper reports its own version.
    let mut f = Fake::happy();
    f.marker = true;
    f.hellos.push_back(hello(PROTOCOL_VERSION + 1, true, Some(r"Z:\g\overlay.dll")));
    assert_eq!(run(&mut f, &Config::default()), Outcome::VersionMismatch);
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
}

#[test]
fn marker_present_waits_for_the_pinned_bootstrap_without_hooking_or_waking() {
    let mut f = Fake::happy();
    f.marker = true;
    assert_eq!(run(&mut f, &Config::default()), Outcome::BootstrapTimeout);
    assert_eq!(f.now, 10_000);
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook") || c == "post_wakeups"));
    assert!(!f.log.contains(&"find_ui_thread".to_string()));
}

#[test]
fn the_game_exiting_before_its_window_appears_is_game_exited() {
    let mut f = Fake::happy();
    f.ui = None;
    f.alive_until = Some(3_000);
    assert_eq!(run(&mut f, &Config::default()), Outcome::GameExited);
    assert!(f.now < 120_000, "waited {} ms for a game that had exited", f.now);
}

#[test]
fn the_game_exiting_during_the_hello_wait_is_game_exited() {
    let mut f = Fake::happy();
    f.alive_until = Some(3_000);
    assert_eq!(run(&mut f, &Config::default()), Outcome::GameExited);
    assert!(f.now < 10_000, "waited {} ms for a game that had exited", f.now);
    assert!(!f.log.contains(&"set_hook CallWndProc".to_string()), "no fallback into a dead game");
}

#[test]
fn the_game_exiting_while_waiting_for_the_pinned_bootstrap_is_game_exited() {
    let mut f = Fake::happy();
    f.marker = true;
    f.alive_until = Some(3_000);
    assert_eq!(run(&mut f, &Config::default()), Outcome::GameExited);
    assert!(f.now < 10_000, "waited {} ms for a game that had exited", f.now);
}

/// Catches `game_exited` sent as an `error` (design 2a: it is an event of its own).
#[test]
fn game_exited_maps_to_its_json_event() {
    assert_eq!(Outcome::GameExited.to_event(), Event::GameExited);
}
