#![cfg(unix)]
//! End-to-end tests of the helper under plain Wine (system `/usr/bin/wine`, not Proton).
//! A `uw-test-game` window runs in a scratch prefix; `uw-helper64.exe` runs in the same prefix
//! (same wineserver), hooks the game's UI thread, serves the pipe the bootstrap connects to, and
//! relays JSON lines (commands on its stdin, events on its stdout).
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
//!   xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE` overrides the `wine` binary (default `/usr/bin/wine`); `UW_WINESERVER` the
//! `wineserver` (default `/usr/bin/wineserver`). The scratch prefix goes under the system temp.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// A running `uw-helper64.exe`: its JSON-lines events arrive on a channel from a reader thread,
/// its stderr diagnostics accumulate in a shared buffer (so a test can assert which hook fired or
/// that the hook was skipped), commands go to its stdin. Dropping it kills the helper.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Helper {
    /// The next JSON-lines event, or a test failure after `within`.
    fn next_event(&self, within: Duration) -> String {
        self.events.recv_timeout(within).unwrap_or_else(|_| panic!("no helper event within {within:?}"))
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("helper stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
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
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
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
    Helper { child, stdin, events: rx, stderr: diag }
}

fn load_overlay_line(path_wine: &str) -> String {
    format!(r#"{{"cmd":"load_overlay","path":"{}"}}"#, path_wine.replace('\\', "\\\\"))
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
        helper.stderr.lock().unwrap()
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
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));

    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"already_loaded""#));
    helper.send(&load_overlay_line(&other));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"already_loaded_other""#));
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
        assert!(first.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
        first.send(&load_overlay_line(&overlay));
        assert!(first.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
    }

    // Helper 2: the marker mutex exists, so it skips the hook, and the pinned bootstrap reconnects
    // with overlay_loaded: true (design "Reconnect after a client restart").
    let second = start_helper(&pfx, &[], &[]);
    let connected = second.next_event(Duration::from_secs(60));
    assert!(connected.contains("bootstrap_connected"), "got: {connected}");
    assert!(connected.contains(r#""overlay_loaded":true"#), "got: {connected}");
    // It reused the pinned bootstrap without re-hooking (design: "no new hook").
    assert!(second.wait_stderr("marker present; skipping hook", Duration::from_secs(5)));

    // The overlay was not loaded a second time: still exactly one marker file.
    assert_eq!(markers(&pfx, "uw-test-overlay-").len(), 1);
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn forced_callwndproc_fallback_also_connects_and_loads() {
    let mut pfx = setup("fallback");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &["--fallback"], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    // The connection must have come through WH_CALLWNDPROC, not a WH_GETMESSAGE fallback after a
    // timeout (design "forced WH_CALLWNDPROC"; the wake-ups make it fire — I1).
    assert!(helper.wait_stderr("hooked WH_CALLWNDPROC", Duration::from_secs(5)));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_game_without_a_window_reports_no_window() {
    let mut pfx = setup("nowindow");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--no-window").spawn().unwrap(),
    );
    // A short window timeout instead of the 120 s default; the prefix boot still counts against
    // the overall event deadline below.
    let helper = start_helper(&pfx, &[], &[("UW_WINDOW_TIMEOUT_MS", "8000")]);
    let event = helper.next_event(Duration::from_secs(200));
    assert!(event.contains(r#""event":"error""#) && event.contains(r#""code":"no_window""#), "got: {event}");
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn an_overlay_without_the_export_reports_no_start_export() {
    let mut pfx = setup("noexport");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    // The bootstrap DLL exists and loads but has no `uw_overlay_start` export.
    let no_export = to_wine_path(&win_artifact("uw_bootstrap64.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&no_export));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"no_start_export""#));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn killing_the_game_reports_game_exited() {
    let mut pfx = setup("gameexit");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));

    // Kill the game while the helper is idle on stdin; the watcher thread must notice and report
    // `game_exited` (design failure table), without waiting for the next stdin command.
    if let Some(mut g) = pfx.game.take() {
        let _ = g.kill();
        wait_bounded(&mut g, Duration::from_secs(10));
    }
    let event = helper.next_event(Duration::from_secs(20));
    assert!(event.contains(r#""event":"error""#) && event.contains(r#""code":"game_exited""#), "got: {event}");
}
