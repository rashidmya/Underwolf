#![cfg(unix)]
//! End-to-end tests of the helper under plain Wine (system `/usr/bin/wine`, not Proton).
//! A `uw-test-game` window runs in a scratch prefix; `uw-helper64.exe` runs in the same prefix
//! (same wineserver), hooks the game's UI thread, serves the pipe the bootstrap connects to, and
//! relays JSON lines (commands on its stdin, events on its stdout).
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-helper --example overlay_probe
//!   xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE` overrides the `wine` binary (default `/usr/bin/wine`); `UW_WINESERVER` the
//! `wineserver` (default `/usr/bin/wineserver`). The scratch prefix goes under the system temp.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::{Command as Cmd, Event};

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}

fn win_artifact(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release")
        .join(name);
    assert!(p.exists(), "missing {} (build the Windows artifacts first)", p.display());
    p
}

fn to_wine_path(p: &Path) -> String {
    format!("Z:{}", p.to_string_lossy().replace('/', "\\"))
}

struct Prefix {
    dir: PathBuf,
    game: Option<Child>,
}

/// Wait up to `d` for `child` to exit, then kill it. Every teardown wait is bounded, so a stuck
/// Wine process fails or slows a test instead of hanging the whole suite.
fn wait_bounded(child: &mut Child, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for Prefix {
    fn drop(&mut self) {
        if let Some(mut g) = self.game.take() {
            let _ = g.kill();
            wait_bounded(&mut g, Duration::from_secs(10));
        }
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.dir).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap_or(&self.dir));
    }
}

fn setup(tag: &str) -> Prefix {
    let dir = std::env::temp_dir()
        .join(format!("uw-inject-{tag}-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&dir).unwrap();
    Prefix { dir, game: None }
}

/// Runs a Windows program in the game's scratch prefix (same wineserver as the game).
/// `WINEDLLOVERRIDES=mscoree,mshtml=` disables the Mono/Gecko installer prompts on a fresh prefix.
fn wine_cmd(pfx: &Prefix, program: &Path) -> Command {
    let mut c = Command::new(wine());
    c.arg(program)
        .env("WINEPREFIX", &pfx.dir)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=");
    c
}

/// Files starting with `stem` in the prefix users' `%TEMP%`
/// (`drive_c/users/<user>/AppData/Local/Temp`). This is what the design means by "read it through
/// the prefix". **No recursion:** a Wine prefix has `dosdevices/z: -> /`, so a recursive walk that
/// follows directory symlinks escapes into the whole filesystem (and into every other prefix's
/// `z:`) and never finishes — which hangs the test and skips its `Drop` cleanup.
fn markers(pfx: &Prefix, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for user in std::fs::read_dir(pfx.dir.join("drive_c/users")).into_iter().flatten().flatten() {
        let temp = user.path().join("AppData/Local/Temp");
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(stem) {
                out.push(e.path());
            }
        }
    }
    out
}

/// The one overlay marker in `pfx` and its mtime. The marker is `uw-test-overlay-<pid>.txt`,
/// rewritten (`CREATE_ALWAYS`, same text) on every load in the same game, so a second load keeps
/// the count at 1 but moves the mtime: compare stamps to prove "not loaded again".
fn marker_stamp(pfx: &Prefix) -> (PathBuf, SystemTime) {
    let found = markers(pfx, "uw-test-overlay-");
    assert_eq!(found.len(), 1, "expected exactly one overlay marker, got {found:?}");
    let modified = std::fs::metadata(&found[0]).and_then(|m| m.modified()).unwrap();
    (found[0].clone(), modified)
}

/// Wait up to `within` until the game runs in `pfx`: a process whose argv0 basename is
/// `uw-test-game.exe` (Wine sets argv0 to the Windows path) and whose environment has this
/// prefix's `WINEPREFIX`. Then the helper's window timeout measures only "found, no window".
fn wait_game_in(pfx: &Prefix, within: Duration) {
    let want = format!("WINEPREFIX={}", pfx.dir.display());
    let running = || {
        std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
            let proc_dir = e.path();
            let named = std::fs::read(proc_dir.join("cmdline")).is_ok_and(|c| {
                let argv0 = String::from_utf8_lossy(c.split(|&b| b == 0).next().unwrap_or_default()).into_owned();
                argv0.rsplit(['\\', '/']).next().is_some_and(|b| b.eq_ignore_ascii_case("uw-test-game.exe"))
            });
            named
                && std::fs::read(proc_dir.join("environ"))
                    .is_ok_and(|env| env.split(|&b| b == 0).any(|v| v == want.as_bytes()))
        })
    };
    let start = Instant::now();
    while !running() {
        assert!(start.elapsed() < within, "uw-test-game.exe never started in {}", pfx.dir.display());
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A running `uw-helper64.exe`: its JSON-lines events arrive on a channel from a reader thread,
/// its stderr diagnostics accumulate in a shared buffer (so a test can assert which hook fired or
/// that the hook was skipped), commands go to its stdin. The game-data channel's events (design
/// 2b) arrive on a channel of their own, `data`, so the control-channel tests never see them.
/// While a test holds `stdout_gate`, the reader stops reading (a host slow to read). Dropping it
/// kills the helper.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    data: Receiver<String>,
    stdout_gate: Arc<Mutex<()>>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Helper {
    /// The next JSON-lines event, or a test failure (with the helper's stderr) after `within`.
    fn next_any_event(&self, within: Duration) -> String {
        self.events
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no helper event within {within:?}; stderr: {:?}", self.stderr_lines()))
    }

    /// The next event other than the overlay's own `overlay_connected`/`overlay_disconnected`.
    /// Since plan 2a the loaded test overlay connects on its own pipe, in parallel with the
    /// bootstrap's events, and the bootstrap-level tests do not look at it.
    fn next_event(&self, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let event = self.next_any_event(deadline.saturating_duration_since(Instant::now()));
            if !event.contains(r#""event":"overlay_connected""#) && !event.contains(r#""event":"overlay_disconnected""#) {
                return event;
            }
        }
    }

    /// The next event, which must contain `needle`. A mismatch fails with the event and the
    /// helper's stderr, so a Wine flake can be diagnosed from the test output alone.
    fn expect_event(&self, needle: &str, within: Duration) -> String {
        let event = self.next_event(within);
        assert!(event.contains(needle), "expected {needle} in: {event}; stderr: {:?}", self.stderr_lines());
        event
    }

    /// A copy of the stderr lines so far (copied, so a failing assert never panics while holding
    /// the lock the reader thread needs).
    fn stderr_lines(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("helper stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next event, parsed (a line that is not an event fails the test).
    fn next_parsed(&self, within: Duration) -> Event {
        let line = self.next_any_event(within);
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"))
    }

    /// Read events until each of `want` has been seen, in any order (two sources, such as the
    /// bootstrap's `status` and the overlay's `overlay_connected`, race). Returns all seen.
    fn expect_all(&self, want: &[Event], within: Duration) -> Vec<Event> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        while !want.iter().all(|w| seen.contains(w)) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "missing {want:?}; saw {seen:?}; stderr: {:?}", self.stderr_lines());
            seen.push(self.next_parsed(left));
        }
        seen
    }

    /// No event at all within `d`.
    fn expect_quiet(&self, d: Duration) {
        if let Ok(line) = self.events.recv_timeout(d) {
            panic!("expected no event, got {line}");
        }
    }

    /// Close the helper's stdin, as a host that exits or dies does.
    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    /// Wait up to `within` for the helper to exit by itself; its status, or `None` if it did not.
    fn wait_exit(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let start = Instant::now();
        while start.elapsed() < within {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    fn send_cmd(&mut self, cmd: &Cmd) {
        let line = cmd.to_line();
        self.send(&line);
    }

    /// The next game-data event, parsed (a line that is not an event fails the test).
    fn next_data(&self, within: Duration) -> Event {
        let line = self
            .data
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no data event within {within:?}; stderr: {:?}", self.stderr_lines()));
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"))
    }

    /// Throw away the game-data events read so far.
    fn drain_data(&self) {
        while self.data.try_recv().is_ok() {}
    }

    /// No game-data event within `d`.
    fn expect_data_quiet(&self, d: Duration) {
        if let Ok(line) = self.data.recv_timeout(d) {
            panic!("expected no data event, got {line}");
        }
    }

    /// Wait up to `within` for a stderr diagnostic line containing `needle`.
    fn wait_stderr(&self, needle: &str, within: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.stderr.lock().unwrap().iter().any(|l| l.contains(needle)) {
                return true;
            }
            if start.elapsed() >= within {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        wait_bounded(&mut self.child, Duration::from_secs(10));
    }
}

/// Whether an event line is the game-data channel's (design 2b): its five events, and the errors
/// whose code starts with `data_`.
fn is_data_line(line: &str) -> bool {
    match Event::parse(line) {
        Ok(
            Event::DataConnected { .. }
            | Event::Info { .. }
            | Event::DataEvent { .. }
            | Event::DataLost { .. }
            | Event::DataDisconnected { .. },
        ) => true,
        Ok(Event::Error { code, .. }) => code.starts_with("data_"),
        _ => false,
    }
}

/// Start the helper against `uw-test-game.exe` in `pfx`, with extra args and environment.
fn start_helper(pfx: &Prefix, extra: &[&str], envs: &[(&str, &str)]) -> Helper {
    let mut cmd = wine_cmd(pfx, &win_artifact("uw-helper64.exe"));
    cmd.arg("uw-test-game.exe")
        .arg("--bootstrap")
        .arg(to_wine_path(&win_artifact("uw_bootstrap64.dll")))
        .args(extra)
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    let (data_tx, data_rx) = channel();
    let stdout_gate = Arc::new(Mutex::new(()));
    let gate = Arc::clone(&stdout_gate);
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            // Wait here while a test holds the gate.
            drop(gate.lock().unwrap());
            let Some(Ok(line)) = lines.next() else { break };
            if !line.trim_start().starts_with('{') {
                continue;
            }
            let to = if is_data_line(&line) { &data_tx } else { &tx };
            if to.send(line).is_err() {
                break;
            }
        }
    });
    let diag = Arc::new(Mutex::new(Vec::new()));
    let diag2 = Arc::clone(&diag);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            diag2.lock().unwrap().push(line);
        }
    });
    Helper { child, stdin, events: rx, data: data_rx, stdout_gate, stderr: diag }
}

fn load_overlay_line(path_wine: &str) -> String {
    format!(r#"{{"cmd":"load_overlay","request_id":1,"path":"{}"}}"#, path_wine.replace('\\', "\\\\"))
}

/// Drive the helper over JSON lines: connect, load the test overlay, expect `ok` and the marker.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn helper_connects_loads_overlay_and_reports_ok() {
    let mut pfx = setup("happy");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    // The first run boots the prefix, so allow up to 200 s for the connection.
    let connected = helper.next_event(Duration::from_secs(200));
    assert!(connected.contains(r#""event":"bootstrap_connected""#), "got: {connected}");
    // The happy path connects through the primary hook, not the fallback.
    assert!(
        helper.wait_stderr("hooked WH_GETMESSAGE", Duration::from_secs(5)),
        "expected the WH_GETMESSAGE hook; stderr: {:?}",
        helper.stderr_lines()
    );
    // `hooked …` is logged when the hook is set, not when it fires: if WH_GETMESSAGE never fired,
    // the WH_CALLWNDPROC fallback would connect instead. Both lines precede the event, so 1 s of
    // grace for the stderr reader is plenty.
    assert!(
        !helper.wait_stderr("hooked WH_CALLWNDPROC", Duration::from_secs(1)),
        "connected only through the WH_CALLWNDPROC fallback; stderr: {:?}",
        helper.stderr_lines()
    );

    helper.send(&load_overlay_line(&overlay));
    let status = helper.next_event(Duration::from_secs(30));
    assert!(status.contains(r#""event":"status""#) && status.contains(r#""code":"ok""#), "got: {status}");

    // The overlay wrote its marker file; find it by walking the prefix.
    assert_eq!(markers(&pfx, "uw-test-overlay-").len(), 1, "expected the overlay marker in the prefix");
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn reloading_the_same_path_is_already_loaded_and_another_is_already_loaded_other() {
    let mut pfx = setup("reload");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let other = to_wine_path(&win_artifact("uw_bootstrap64.dll")); // a different, existing DLL

    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));

    helper.send(&load_overlay_line(&overlay));
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));
    helper.send(&load_overlay_line(&overlay));
    // The closing quote keeps this from matching `already_loaded_other`.
    helper.expect_event(r#""code":"already_loaded""#, Duration::from_secs(30));
    helper.send(&load_overlay_line(&other));
    helper.expect_event(r#""code":"already_loaded_other""#, Duration::from_secs(30));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_new_helper_reuses_the_pinned_bootstrap_without_rehooking() {
    let mut pfx = setup("reconnect");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    // Helper 1: connect and load the overlay. Leaving the block kills it (design: "kill the
    // helper, start a new one"); its pipe and helper mutex go with it.
    {
        let mut first = start_helper(&pfx, &[], &[]);
        first.expect_event("bootstrap_connected", Duration::from_secs(200));
        first.send(&load_overlay_line(&overlay));
        first.expect_event(r#""code":"ok""#, Duration::from_secs(30));
    }
    let before = marker_stamp(&pfx);

    // Helper 2: the marker mutex exists, so it skips the hook, and the pinned bootstrap reconnects
    // with overlay_loaded: true (design "Reconnect after a client restart").
    let second = start_helper(&pfx, &[], &[]);
    let connected = second.expect_event("bootstrap_connected", Duration::from_secs(60));
    assert!(connected.contains(r#""overlay_loaded":true"#), "got: {connected}");
    // It reused the pinned bootstrap without re-hooking (design: "no new hook").
    assert!(
        second.wait_stderr("marker present; skipping hook", Duration::from_secs(5)),
        "expected the hook to be skipped; stderr: {:?}",
        second.stderr_lines()
    );

    // The overlay was not loaded a second time: the same single marker, not rewritten.
    assert_eq!(marker_stamp(&pfx), before, "the overlay marker was rewritten (a second load)");
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn forced_callwndproc_fallback_also_connects_and_loads() {
    let mut pfx = setup("fallback");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &["--fallback"], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    // The connection must have come through WH_CALLWNDPROC, not a WH_GETMESSAGE fallback after a
    // timeout (design "forced WH_CALLWNDPROC"; the wake-ups make it fire). `hooked …` is logged
    // when a hook is set, not when it fires, so also require that WH_GETMESSAGE was never set:
    // with a WH_CALLWNDPROC that cannot fire, the helper would unhook it after the hello timeout
    // and connect through WH_GETMESSAGE. Both lines precede the event; 1 s of grace is plenty.
    assert!(
        helper.wait_stderr("hooked WH_CALLWNDPROC", Duration::from_secs(5)),
        "expected the WH_CALLWNDPROC hook; stderr: {:?}",
        helper.stderr_lines()
    );
    assert!(
        !helper.wait_stderr("hooked WH_GETMESSAGE", Duration::from_secs(1)),
        "connected only through the WH_GETMESSAGE fallback; stderr: {:?}",
        helper.stderr_lines()
    );
    helper.send(&load_overlay_line(&overlay));
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_game_without_a_window_reports_no_window() {
    let mut pfx = setup("nowindow");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--no-window").spawn().unwrap(),
    );
    // `run` also reports `no_window` when it never *finds* the game within the window timeout.
    // Wait until the game runs in this prefix (the first run boots the prefix) before starting the
    // helper, so the short timeout below only covers "found, but no window".
    wait_game_in(&pfx, Duration::from_secs(180));
    // A short window timeout instead of the 120 s default.
    let helper = start_helper(&pfx, &[], &[("UW_WINDOW_TIMEOUT_MS", "8000")]);
    let event = helper.expect_event(r#""code":"no_window""#, Duration::from_secs(60));
    assert!(event.contains(r#""event":"error""#), "got: {event}");
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn an_overlay_without_the_export_reports_no_start_export() {
    let mut pfx = setup("noexport");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    // The bootstrap DLL exists and loads but has no `uw_overlay_start` export.
    let no_export = to_wine_path(&win_artifact("uw_bootstrap64.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send(&load_overlay_line(&no_export));
    helper.expect_event(r#""code":"no_start_export""#, Duration::from_secs(30));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn killing_the_game_reports_game_exited() {
    let mut pfx = setup("gameexit");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send(&load_overlay_line(&overlay));
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));
    // The data thread relays the test producer (design 2b "Failures": `game_exited` still stops
    // everything).
    assert!(matches!(helper.next_data(Duration::from_secs(10)), Event::DataConnected { .. }));

    // Kill the game while the helper is idle; the watcher thread must notice and report
    // `game_exited` (design failure table), without waiting for the next stdin command. The relay
    // may see the bootstrap pipe drop first (`bootstrap_disconnected`); nothing else may come.
    if let Some(mut g) = pfx.game.take() {
        let _ = g.kill();
        wait_bounded(&mut g, Duration::from_secs(10));
    }
    let mut event = helper.next_event(Duration::from_secs(20));
    if event == r#"{"event":"bootstrap_disconnected"}"# {
        event = helper.next_event(Duration::from_secs(20));
    }
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}

// --- The control channel (plan 2a) ---

const OVERLAY_VERSION: u32 = 1;

/// The test overlay's last applied state (`uw-test-state-<pid>.txt`), once it has written one.
fn overlay_state(pfx: &Prefix) -> Option<String> {
    let found = markers(pfx, "uw-test-state-");
    found.first().and_then(|p| std::fs::read_to_string(p).ok())
}

fn load(path: &str) -> Cmd {
    Cmd::LoadOverlay { request_id: 1, path: path.into() }
}

fn ok(request_id: u32) -> Event {
    Event::Status { request_id, code: "ok".into(), detail: 0, message: "overlay started".into() }
}

fn applied(applied_seq: u64) -> Event {
    Event::Bell { bits: vec!["state_applied".into()], applied_seq }
}

/// Connect, load the test overlay, and wait for both its `status ok` and its own connection.
fn start_with_overlay(pfx: &Prefix) -> Helper {
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));
    helper
}

/// Catches the design's main flow breaking anywhere: the overlay not connecting on its own pipe,
/// `set_state` not reaching the game through the state block and `bell-game`, the overlay's
/// `state_applied` not coming back through `bell-host`, `applied_seq` not matching `seq`, or an
/// unchanged `set_state` bumping `seq` or ringing.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn the_overlay_connects_and_applies_set_state() {
    let mut pfx = setup("state");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);

    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 2, seq: 1 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(1));
    assert_eq!(overlay_state(&pfx).as_deref(), Some("seq=1 edit_mode=1 visible=0\n"));

    // Nothing changes: the same seq, and no bell.
    helper.send_cmd(&Cmd::SetState { request_id: 3, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 3, seq: 1 });
    helper.expect_quiet(Duration::from_secs(2));

    helper.send_cmd(&Cmd::SetState { request_id: 4, edit_mode: Some(false), visible: Some(true) });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 4, seq: 2 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(2));
    assert_eq!(overlay_state(&pfx).as_deref(), Some("seq=2 edit_mode=0 visible=1\n"));
}

/// Catches `ping` not reaching the overlay, its `pong` not coming back with the request's id and
/// the last applied seq, or a ping with no overlay being sent anyway instead of refused.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn ping_gets_pong_with_the_last_applied_seq() {
    let mut pfx = setup("ping");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));

    // No overlay yet: refused at once (design: no queueing while disconnected).
    helper.send_cmd(&Cmd::Ping { request_id: 9 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::reply_error(9, "overlay_not_connected"));

    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));
    helper.send_cmd(&Cmd::Ping { request_id: 10 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 10, seen_seq: 0 });
    helper.send_cmd(&Cmd::SetState { request_id: 11, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 11, seq: 1 }, applied(1)], Duration::from_secs(10));
    helper.send_cmd(&Cmd::Ping { request_id: 12 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 12, seen_seq: 1 });
}

/// Catches plan 1's final-review M2: a `status` that comes after its request timed out must be
/// discarded, not taken as the reply to the next `load_overlay`. Also catches commands sent before
/// the bootstrap connects being queued (and run later) instead of refused at once (design: no
/// queueing while disconnected).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_slow_overlay_start_times_out_and_its_late_status_is_discarded() {
    let mut pfx = setup("slow");
    // The overlay's start takes 6 s; the helper waits only 2 s for a status.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_START_DELAY_MS", "6000").spawn().unwrap(),
    );
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[("UW_STATUS_TIMEOUT_MS", "2000")]);
    // Sent while the helper still hooks: refused, and answered before `bootstrap_connected`.
    helper.send_cmd(&Cmd::LoadOverlay { request_id: 7, path: overlay.clone() });
    helper.send_cmd(&Cmd::SetState { request_id: 8, edit_mode: Some(true), visible: None });
    helper.send_cmd(&Cmd::Ping { request_id: 9 });
    let first: Vec<Event> = (0..4).map(|_| helper.next_parsed(Duration::from_secs(200))).collect();
    assert_eq!(
        first[..3],
        [
            Event::reply_error(7, "bootstrap_not_connected"),
            Event::reply_error(8, "overlay_not_connected"),
            Event::reply_error(9, "overlay_not_connected"),
        ],
        "all: {first:?}"
    );
    assert!(matches!(first[3], Event::BootstrapConnected { overlay_loaded: false, .. }), "{first:?}");

    helper.send_cmd(&load(&overlay));
    let timeout = Event::parse(&helper.next_event(Duration::from_secs(10))).unwrap();
    assert_eq!(timeout, Event::reply_error(1, "status_timeout"));
    // The start finishes about 4 s later; its `status` (request 1) is discarded.
    assert!(
        helper.wait_stderr("discarding a status for request 1", Duration::from_secs(15)),
        "the late status was not discarded; stderr: {:?}",
        helper.stderr_lines()
    );
    // The next request gets its own reply: the overlay did load, so `already_loaded`.
    helper.send_cmd(&Cmd::LoadOverlay { request_id: 2, path: overlay });
    let reply = Event::parse(&helper.next_event(Duration::from_secs(10))).unwrap();
    assert_eq!(
        reply,
        Event::Status { request_id: 2, code: "already_loaded".into(), detail: 0, message: "overlay already loaded".into() }
    );
}

/// Catches owner decision D1 breaking: a `ping` the overlay does not answer in time must get
/// `overlay_not_responding` (and no other reply), its late `pong` must be discarded, and the next
/// `ping` must be answered.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_ping_the_overlay_does_not_answer_in_time_is_overlay_not_responding() {
    let mut pfx = setup("pingslow");
    // Every bell takes the overlay 5 s, and its worker answers no ping meanwhile; the helper
    // waits 2 s for a pong.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_BELL_DELAY_MS", "5000").spawn().unwrap(),
    );
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[("UW_PING_TIMEOUT_MS", "2000")]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));

    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 2, seq: 1 });
    helper.send_cmd(&Cmd::Ping { request_id: 3 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::reply_error(3, "overlay_not_responding"));
    // The bell is applied about 3 s later; then the late pong comes and is discarded.
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(1));
    assert!(
        helper.wait_stderr("discarding a pong for 3", Duration::from_secs(10)),
        "the late pong was not discarded; stderr: {:?}",
        helper.stderr_lines()
    );
    helper.send_cmd(&Cmd::Ping { request_id: 4 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 4, seen_seq: 1 });
}

/// Catches a state block from another layout being used or rewritten instead of refused (design
/// "Messages and layouts": `version_mismatch`, "restart the game"), or the game hooked anyway.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_foreign_state_block_is_version_mismatch() {
    let mut pfx = setup("badblock");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--bad-state-block").spawn().unwrap());
    wait_game_in(&pfx, Duration::from_secs(180));
    let mut helper = start_helper(&pfx, &[], &[]);
    let event = Event::parse(&helper.next_event(Duration::from_secs(60))).unwrap();
    assert_eq!(event, Event::error("version_mismatch"));
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(1), "stderr: {:?}", helper.stderr_lines());
    assert!(
        helper.wait_stderr("state block layout 2, not 1; restart the game", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    assert!(!helper.stderr_lines().iter().any(|l| l.contains("hooked")), "the game was hooked anyway");
}

/// Catches a helper that outlives its host while it still waits for the game's window (design
/// "Failures": it exits when its stdin closes, in every phase, so no orphan keeps the helper mutex).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn closing_stdin_ends_the_helper_while_it_waits_for_a_window() {
    let mut pfx = setup("stdinwait");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--no-window").spawn().unwrap());
    wait_game_in(&pfx, Duration::from_secs(180));
    // With no window the helper would wait 120 s (the default) before `no_window`.
    let mut helper = start_helper(&pfx, &[], &[]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(helper.child.try_wait().unwrap().is_none(), "the helper ended early: {:?}", helper.stderr_lines());
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "stderr: {:?}", helper.stderr_lines());
    assert!(helper.wait_stderr("stdin closed; exiting", Duration::from_secs(2)));
}

/// The same once the bootstrap is connected and the relay runs, and while the data thread relays
/// the test producer (design 2b "Failures": stdin closing ends the helper in every phase).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn closing_stdin_ends_the_helper_after_it_connected() {
    let mut pfx = setup("stdinrelay");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    assert!(matches!(helper.next_data(Duration::from_secs(10)), Event::DataConnected { .. }));
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "stderr: {:?}", helper.stderr_lines());
}

/// Version the `overlay_probe` example reports in its `overlay_hello`.
const PROBE_VERSION: u32 = 77;

/// `examples/overlay_probe.exe` running in the game's prefix: a scripted overlay-pipe client whose
/// stdout lines arrive on a channel. Dropping it kills it.
struct Probe {
    child: Child,
    lines: Receiver<String>,
}

impl Probe {
    fn start(pfx: &Prefix, args: &[&str]) -> Probe {
        let mut child = wine_cmd(pfx, &win_artifact("examples/overlay_probe.exe"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Probe { child, lines: rx }
    }

    fn expect_line(&self, needle: &str, within: Duration) -> String {
        let line = self.lines.recv_timeout(within).unwrap_or_else(|_| panic!("probe: no line within {within:?}"));
        assert!(line.contains(needle), "probe: expected {needle}, got {line}");
        line
    }

    fn expect_quiet(&self, d: Duration) {
        if let Ok(line) = self.lines.recv_timeout(d) {
            panic!("probe: expected nothing, got {line}");
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        wait_bounded(&mut self.child, Duration::from_secs(10));
    }
}

/// Catches the overlay hello rule breaking (design "Messages and layouts"): a first message that is
/// not a supported `overlay_hello` must be refused with `pipe_error` (not a hello) or
/// `version_mismatch` (an unsupported state layout) and the connection dropped, with no
/// `overlay_connected`. After a good hello, a message that is garbage or over the size limit must
/// give `pipe_error`, drop the overlay, and leave the helper listening for a new connection.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn the_overlay_hello_rule_and_bad_messages_drop_the_overlay() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("hello");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    {
        let p = Probe::start(&pfx, &["bad-first"]);
        assert_eq!(helper.next_parsed(Duration::from_secs(60)), Event::error("pipe_error"));
        p.expect_line("dropped", Duration::from_secs(10));
    }
    {
        let p = Probe::start(&pfx, &["bad-version"]);
        assert_eq!(helper.next_parsed(Duration::from_secs(60)), Event::error("version_mismatch"));
        p.expect_line("dropped", Duration::from_secs(10));
    }
    helper.expect_quiet(Duration::from_secs(1));
    for mode in ["ok-then-bad", "oversize"] {
        let p = Probe::start(&pfx, &[mode]);
        assert_eq!(helper.next_parsed(Duration::from_secs(60)), Event::OverlayConnected { module_version: PROBE_VERSION });
        assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::error("pipe_error"));
        assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::OverlayDisconnected);
        p.expect_line("dropped", Duration::from_secs(10));
        // The helper listens again.
        assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::OverlayConnected { module_version: PROBE_VERSION });
        p.expect_line("reconnected", Duration::from_secs(10));
        drop(p);
        assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::OverlayDisconnected);
    }
}

/// Catches `set_state` treating a lock timeout as acquired (it must answer `state_busy` after
/// about 100 ms), a bell read under a busy lock being lost (its bits must go back and ring again),
/// a `set_state` that changes nothing bumping `seq` or ringing `bell-game`, and a `set_state` with
/// no overlay connected not being refused.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn set_state_is_busy_under_the_lock_and_a_busy_bell_is_retried() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("busy");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send_cmd(&Cmd::SetState { request_id: 1, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::reply_error(1, "overlay_not_connected"));

    // The probe holds the state lock for 3 s.
    let p = Probe::start(&pfx, &["hold", "3000"]);
    assert_eq!(helper.next_parsed(Duration::from_secs(60)), Event::OverlayConnected { module_version: PROBE_VERSION });
    p.expect_line("locked", Duration::from_secs(10));
    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(2)), Event::reply_error(2, "state_busy"));
    p.expect_line("released", Duration::from_secs(10));
    // The bell rung while the lock was held comes out once, after the release.
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), applied(42));
    helper.expect_quiet(Duration::from_secs(1));

    helper.send_cmd(&Cmd::SetState { request_id: 3, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), Event::StateSet { request_id: 3, seq: 1 });
    p.expect_line("bell 1 seq 1", Duration::from_secs(5));
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), applied(1));

    // Nothing changes: the current seq, no bump, no bell-game ring.
    helper.send_cmd(&Cmd::SetState { request_id: 4, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), Event::StateSet { request_id: 4, seq: 1 });
    p.expect_quiet(Duration::from_millis(1500));
    helper.expect_quiet(Duration::from_millis(500));

    helper.send_cmd(&Cmd::SetState { request_id: 5, edit_mode: Some(false), visible: Some(true) });
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), Event::StateSet { request_id: 5, seq: 2 });
    p.expect_line("bell 6 seq 2", Duration::from_secs(5));
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), applied(2));
}

// --- The game-data channel (plan 2b) ---

/// The helper with `args` must exit 2 at once and say `why` (design 2b: a malformed data argument
/// is a usage error, like the helper's other arguments).
fn assert_usage_error(pfx: &Prefix, args: &[&str], why: &str) {
    let mut h = start_helper(pfx, args, &[]);
    let status = h.wait_exit(Duration::from_secs(30));
    assert_eq!(status.and_then(|s| s.code()), Some(2), "{args:?}: stderr: {:?}", h.stderr_lines());
    assert!(h.wait_stderr(why, Duration::from_secs(2)), "{args:?}: stderr: {:?}", h.stderr_lines());
}

/// Catches a data channel of another format version being read (design 2b "Failures": `error
/// data_version_mismatch`, `detail` the channel id, once per producer, then nothing more from
/// it), the data thread not starting with the relay, and a malformed `--data-channel` or
/// `--data-resume` being accepted.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_data_channel_of_another_version_is_data_version_mismatch_once() {
    let mut pfx = setup("dataversion");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).args(["--bad-data-channel", "version"]).spawn().unwrap(),
    );
    wait_game_in(&pfx, Duration::from_secs(180));
    assert_usage_error(&pfx, &["--data-channel", "x"], "--data-channel needs a channel number");
    assert_usage_error(&pfx, &["--data-resume", "7"], r#"--data-resume: "7" is not <producer_id>:<seq>"#);
    assert_usage_error(&pfx, &["--data-resume"], "--data-resume needs <producer_id>:<seq>");

    let helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    assert_eq!(helper.next_data(Duration::from_secs(10)), Event::data_error("data_version_mismatch", 1));
    assert!(
        helper.wait_stderr("data channel 1: format version 2, not 1", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    // Once per producer: it now only waits for `disconnect`.
    helper.expect_data_quiet(Duration::from_secs(3));
}

/// Catches a data channel whose layout does not fit being read (design 2b "Failures": `error
/// data_bad_layout`, `detail` the channel id, once per producer, then nothing more from it).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_data_channel_with_a_bad_layout_is_data_bad_layout_once() {
    let mut pfx = setup("datalayout");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).args(["--bad-data-channel", "layout"]).spawn().unwrap(),
    );
    let helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    assert_eq!(helper.next_data(Duration::from_secs(10)), Event::data_error("data_bad_layout", 1));
    assert!(
        helper.wait_stderr("data channel 1: hello block magic is not UWDH", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    helper.expect_data_quiet(Duration::from_secs(3));
}

/// The test producer's schema (`uw-test-overlay`).
fn test_schema() -> Vec<SchemaCategory> {
    vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])]
}

/// Read data events until `done` holds for one; return them all.
fn data_until(helper: &Helper, within: Duration, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "data: no match within {within:?}; saw {seen:?}");
        let e = helper.next_data(left);
        let stop = done(&e);
        seen.push(e);
        if stop {
            return seen;
        }
    }
}

/// The seqs of the `data_event`s in `events`, in order.
fn event_seqs(events: &[Event]) -> Vec<u64> {
    events.iter().filter_map(|e| if let Event::DataEvent { seq, .. } = e { Some(*seq) } else { None }).collect()
}

/// Expect `data_connected` with the test schema, then the two-key snapshot. Returns the producer
/// id, `from_seq`, and the snapshot's `ticks` and `last_seq` values.
fn expect_test_producer(helper: &Helper) -> (u64, u64, String, Option<String>) {
    let connected = helper.next_data(Duration::from_secs(10));
    let Event::DataConnected { channel: 1, producer_id, from_seq, schema } = connected else {
        panic!("expected data_connected, got {connected:?}");
    };
    assert_eq!(schema, test_schema());
    let ticks = match helper.next_data(Duration::from_secs(5)) {
        Event::Info { channel: 1, category, key, value: Some(v), .. } if category == "test" && key == "ticks" => v,
        other => panic!("expected the ticks snapshot, got {other:?}"),
    };
    let last_seq = match helper.next_data(Duration::from_secs(5)) {
        Event::Info { channel: 1, key, value, .. } if key == "last_seq" => value,
        other => panic!("expected the last_seq snapshot, got {other:?}"),
    };
    (producer_id, from_seq, ticks, last_seq)
}

/// Catches the design's main data flow breaking at the helper (design 2b flow steps 2-5): data
/// events before any producer exists, no `data_connected` with the test schema once the overlay
/// creates it, a snapshot without both keys (`last_seq` still null), ticks that do not follow
/// `from_seq` with contiguous seqs, `ticks` info not following them, or no `state_seen` (and no
/// `last_seq`) after a `set_state`.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn the_data_thread_waits_for_the_producer_then_relays_it() {
    let mut pfx = setup("data");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    // No producer yet: the data thread looks once a second and says nothing.
    helper.expect_data_quiet(Duration::from_secs(3));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));

    let (_, from_seq, ticks, snapshot_last_seq) = expect_test_producer(&helper);
    assert_eq!(snapshot_last_seq, None, "no state was seen yet");
    let ticks: u64 = ticks.parse().unwrap_or_else(|_| panic!("ticks {ticks:?}"));
    // Two more ticks: each a `tick` event (data: the count), after a `ticks` info with that count.
    let seen = data_until(&helper, Duration::from_secs(10), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "tick" && *data == (ticks + 2).to_string())
    });
    let numbers = |want_event: bool| -> Vec<u64> {
        seen.iter()
            .filter_map(|e| match e {
                Event::DataEvent { name, data, .. } if want_event && name == "tick" => data.parse().ok(),
                Event::Info { key, value: Some(v), .. } if !want_event && key == "ticks" => v.parse().ok(),
                _ => None,
            })
            .collect()
    };
    // Attach reads the snapshot under the info lock and the start point under the events lock, a
    // moment apart, and a tick commits `ticks` just before it writes its event. So the ring may be
    // a tick behind the snapshot (events from `ticks` on) or a tick ahead (only `ticks + 2`).
    let events = numbers(true);
    let fits = [vec![ticks + 1, ticks + 2], vec![ticks, ticks + 1, ticks + 2], vec![ticks + 2]];
    assert!(fits.contains(&events), "{seen:?}");
    assert_eq!(numbers(false), [ticks + 1, ticks + 2], "{seen:?}");

    // `set_state` -> `state_applied` -> a `state_seen` event and `last_seq`, both with seq 1 (the
    // info and the event come in either order).
    fn state_seen(e: &Event) -> bool {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    }
    fn last_seq(e: &Event) -> bool {
        matches!(e, Event::Info { key, value: Some(v), .. } if key == "last_seq" && v == "1")
    }
    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 2, seq: 1 }, applied(1)], Duration::from_secs(10));
    let mut more = data_until(&helper, Duration::from_secs(10), |e| state_seen(e) || last_seq(e));
    let rest: fn(&Event) -> bool = if more.iter().any(state_seen) { last_seq } else { state_seen };
    more.extend(data_until(&helper, Duration::from_secs(5), rest));
    let all = [seen, more].concat();
    let seqs = event_seqs(&all);
    let want: Vec<u64> = (from_seq + 1..).take(seqs.len()).collect();
    assert_eq!(seqs, want, "seqs skip or repeat: {all:?}");
}

/// Catches a producer that goes away not reported (design 2b flow step 6: `data_disconnected`),
/// or the data thread not closing everything and going back to waiting: the test producer, made
/// again in the same game, would then fail with `Exists` for good, and no second `data_connected`
/// (a new `producer_id`, the full snapshot, ticks from its own `from_seq`) would come.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_producer_that_goes_away_is_data_disconnected_and_the_next_one_connects() {
    let mut pfx = setup("datarestart");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe"))
            .env("UW_TEST_OVERLAY_DATA_RESTART_MS", "4000")
            .spawn()
            .unwrap(),
    );
    let helper = start_with_overlay(&pfx);
    let (first, _, _, _) = expect_test_producer(&helper);
    data_until(&helper, Duration::from_secs(15), |e| *e == Event::DataDisconnected { channel: 1 });
    let (second, from_seq, ticks, _) = expect_test_producer(&helper);
    assert_ne!(second, first, "the new producer has its own id");
    assert!(ticks.parse::<u64>().is_ok_and(|t| t >= 3), "the snapshot holds the current count: {ticks:?}");
    let seen =
        data_until(&helper, Duration::from_secs(5), |e| matches!(e, Event::DataEvent { name, .. } if name == "tick"));
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}

/// Catches `--data-resume` not honoured (design 2b "Failures": a new helper sends
/// `data_connected` with `from_seq` the resume seq, the full snapshot, then every event after it,
/// once each), and a resume point for another producer (a new game, a reused pid) not starting at
/// the write head.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_new_helper_resumes_after_the_given_seq_only_for_the_same_producer() {
    let mut pfx = setup("dataresume");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let (id, last) = {
        let helper = start_with_overlay(&pfx);
        let (id, from_seq, _, _) = expect_test_producer(&helper);
        data_until(&helper, Duration::from_secs(10), |e| {
            matches!(e, Event::DataEvent { seq, .. } if *seq == from_seq + 2)
        });
        (id, from_seq + 2)
    }; // the helper is killed here
    std::thread::sleep(Duration::from_secs(3)); // ticks go on while no helper runs

    let resume = format!("{id}:{last}");
    let helper = start_helper(&pfx, &["--data-resume", &resume], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(60));
    let (again, from_seq, _, _) = expect_test_producer(&helper);
    assert_eq!((again, from_seq), (id, last), "data_connected must resume at {resume}");
    let seen =
        data_until(&helper, Duration::from_secs(10), |e| matches!(e, Event::DataEvent { seq, .. } if *seq == last + 4));
    assert_eq!(event_seqs(&seen), (last + 1..=last + 4).collect::<Vec<_>>(), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, Event::DataLost { .. })), "{seen:?}");
    drop(helper);

    let other = format!("{}:0", id + 1);
    let helper = start_helper(&pfx, &["--data-resume", &other], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(60));
    let (_, from_seq, _, _) = expect_test_producer(&helper);
    assert!(from_seq >= last + 4, "another producer's resume point must start at the write head, not {from_seq}");
    let seen = data_until(&helper, Duration::from_secs(5), |e| matches!(e, Event::DataEvent { .. }));
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}

/// Catches lines lost, split or reordered while the host reads slowly (design 2b "Failures":
/// `emit()` blocks the writer, and control replies still come), the producer's dropped count not
/// reaching `data_lost` (300 queued at once overflow the queue of 256 by at least 45), or a seq
/// gap not announced by a `data_lost` before the events after it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_burst_while_the_host_reads_slowly_loses_nothing_unannounced() {
    let mut pfx = setup("databurst");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_BURST", "300").spawn().unwrap(),
    );
    let mut helper = start_with_overlay(&pfx);
    let (_, from_seq, _, _) = expect_test_producer(&helper);
    let gate = Arc::clone(&helper.stdout_gate);
    {
        // Stop reading the helper's stdout: its pipe fills and its writes block.
        let _paused = gate.lock().unwrap();
        helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
        helper.send_cmd(&Cmd::Ping { request_id: 3 });
        std::thread::sleep(Duration::from_secs(3));
    }
    // The control replies, late but whole.
    assert_eq!(helper.next_parsed(Duration::from_secs(30)), Event::StateSet { request_id: 2, seq: 1 });
    let rest = [helper.next_parsed(Duration::from_secs(30)), helper.next_parsed(Duration::from_secs(30))];
    assert!(rest.contains(&applied(1)), "{rest:?}");
    assert!(rest.iter().any(|e| matches!(e, Event::Pong { request_id: 3, .. })), "{rest:?}");

    let seen = data_until(&helper, Duration::from_secs(60), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    });
    let (mut prev, mut announced, mut lost, mut gaps) = (from_seq, 0u64, 0u64, 0u64);
    for e in &seen {
        match e {
            Event::DataLost { events, .. } => {
                announced += events;
                lost += events;
            }
            Event::DataEvent { seq, name, data, .. } => {
                let gap = seq.checked_sub(prev + 1).unwrap_or_else(|| panic!("seq {seq} after {prev}"));
                assert!(gap <= announced, "seq {seq} after {prev}: a gap of {gap}, but only {announced} announced");
                if name == "burst" {
                    assert!(data.len() == 1000 && data.parse::<u64>().is_ok(), "a burst event's data: {data:?}");
                }
                (prev, announced, gaps) = (*seq, 0, gaps + gap);
            }
            _ => {}
        }
    }
    assert!(lost - gaps >= 45, "the queue's drops were not reported: lost {lost}, gaps {gaps}");
    // Every other `{` line the helper wrote (the control replies were read above) parses whole.
    while let Ok(line) = helper.events.try_recv() {
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"));
    }
}

/// Catches a producer's last writes lost when it goes away while the helper cannot read (design
/// 2b flow step 6: on `disconnect` the helper reads both regions once more before
/// `data_disconnected`). The host stops reading, so the burst after a state fills the helper's
/// stdout and the data thread blocks in `emit()`; meanwhile the producer flushes its last ticks
/// and goes away. At the next wait `disconnect` wins the tie with both bells, so without that
/// last read neither the last tick nor the last `ticks` would come.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_producer_gone_while_the_host_reads_slowly_still_delivers_its_last_event() {
    let mut pfx = setup("datalast");
    // The producer goes away 6.5 s after it starts, half a second after its sixth tick.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe"))
            .env("UW_TEST_OVERLAY_BURST", "300")
            .env("UW_TEST_OVERLAY_DATA_RESTART_MS", "6500")
            .spawn()
            .unwrap(),
    );
    let mut helper = start_with_overlay(&pfx);
    expect_test_producer(&helper);
    let gate = Arc::clone(&helper.stdout_gate);
    {
        // Stop reading until the producer is surely gone: it started before `data_connected`, so
        // 8 s from here is past its 6.5 s.
        let _paused = gate.lock().unwrap();
        helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
        std::thread::sleep(Duration::from_secs(8));
    }
    let seen = data_until(&helper, Duration::from_secs(60), |e| *e == Event::DataDisconnected { channel: 1 });
    // The last few, each event's data cut short (a burst event's is 1000 bytes).
    let tail: Vec<String> = seen[seen.len().saturating_sub(8)..]
        .iter()
        .map(|e| match e {
            Event::DataEvent { seq, name, data, .. } => format!("{name} #{seq} {data:.8}"),
            other => format!("{other:?}"),
        })
        .collect();
    let last_event = seen.iter().rev().find_map(|e| match e {
        Event::DataEvent { name, data, .. } => Some((name.as_str(), data.as_str())),
        _ => None,
    });
    assert!(last_event == Some(("tick", "6")), "the last event must be tick 6; the tail: {tail:?}");
    let last_ticks = seen.iter().rev().find_map(|e| match e {
        Event::Info { key, value, .. } if key == "ticks" => Some(value.as_deref()),
        _ => None,
    });
    assert_eq!(last_ticks, Some(Some("6")), "the last ticks must be 6; the tail: {tail:?}");
}

/// Catches a busy info lock stalling the data thread or ending it (design 2b "Failures": a lock
/// busy for more than 100 ms is skipped and retried on the next pass), and a value committed
/// meanwhile being lost (design "A busy lock": the producer keeps the newest value and writes it
/// on the next call). The value is `last_seq`, which only a `state_seen` changes, so no tick can
/// overtake it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_busy_info_lock_is_skipped_and_the_newest_value_comes_after() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("databusy");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    expect_test_producer(&helper);
    // The probe holds the info lock for 4 s and rings the info bell every 200 ms.
    let p = Probe::start(&pfx, &["hold-info-lock", "4000"]);
    p.expect_line("locked", Duration::from_secs(60));
    std::thread::sleep(Duration::from_millis(300));
    helper.drain_data();
    // A state applied while the info lock is held: its event gets through, its `last_seq` waits.
    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 2, seq: 1 }, applied(1)], Duration::from_secs(10));
    let held = data_until(&helper, Duration::from_secs(3), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    });
    assert!(!held.iter().any(|e| matches!(e, Event::Info { .. })), "info while the lock was held: {held:?}");
    p.expect_line("released", Duration::from_secs(10));
    data_until(&helper, Duration::from_secs(5), |e| {
        matches!(e, Event::Info { key, value: Some(v), .. } if key == "last_seq" && v == "1")
    });
}

/// Catches a store that stops fitting after `data_connected` being read on, reported again and
/// again, or re-attached (design 2b "Failures": `data_bad_layout` once per producer, then only the
/// wait for `disconnect`), and the next producer not being relayed after it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_store_broken_after_data_connected_is_reported_once_until_disconnect() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("databroken");
    // The producer goes away 15 s after it starts, and a new one comes.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe"))
            .env("UW_TEST_OVERLAY_DATA_RESTART_MS", "15000")
            .spawn()
            .unwrap(),
    );
    let helper = start_with_overlay(&pfx);
    let (first, _, _, _) = expect_test_producer(&helper);
    let p = Probe::start(&pfx, &["break-info-magic"]);
    p.expect_line("broken", Duration::from_secs(60));
    // The next info read finds the broken header.
    data_until(&helper, Duration::from_secs(5), |e| *e == Event::data_error("data_bad_layout", 1));
    assert!(
        helper.wait_stderr("data channel 1: info store magic is not UWDI", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    // Once per producer: nothing more from it, though it keeps ticking.
    helper.expect_data_quiet(Duration::from_secs(3));
    data_until(&helper, Duration::from_secs(20), |e| *e == Event::DataDisconnected { channel: 1 });
    let (second, _, _, _) = expect_test_producer(&helper);
    assert_ne!(second, first, "the next producer is relayed");
}
