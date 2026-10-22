#![cfg(unix)]
//! The whole Linux chain under plain Wine: `uw-inject` finds the test game in a scratch prefix,
//! starts `uw-helper64.exe` in that Wine session, the bootstrap connects, and the test overlay is
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. Ignored by
//! default (needs Wine and the Windows artifacts).
//!
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap \
//!       -p uw-test-overlay -p uw-helper
//!   xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use uw_inject::discovery::find_pids;

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

struct Fixture {
    prefix: PathBuf,
    game: Child,
    /// The game's Linux pid in THIS prefix, passed to `uw-inject --pid` so it never picks a stray
    /// `uw-test-game.exe` from another prefix (the review's leftover-process hazard).
    game_pid: i32,
}
/// Wait up to `d` for `child` to exit, then kill it (bounded teardown, as in the helper tests).
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.game.kill();
        wait_bounded(&mut self.game, Duration::from_secs(10));
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.prefix).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.prefix.parent().unwrap());
    }
}

/// The game's Linux pid running in `prefix`: a `uw-test-game.exe` pid whose `/proc/<pid>/environ`
/// has this prefix's `WINEPREFIX`. `None` until Wine has booted the game.
fn game_pid_in(prefix: &Path) -> Option<i32> {
    let want = format!("WINEPREFIX={}", prefix.display());
    find_pids("uw-test-game.exe").into_iter().find(|pid| {
        std::fs::read(format!("/proc/{pid}/environ"))
            .map(|e| e.split(|&b| b == 0).any(|v| v == want.as_bytes()))
            .unwrap_or(false)
    })
}

/// Start the test game in a fresh prefix and wait until it shows up in `/proc` for this prefix
/// (the first run boots the prefix, which takes a while).
fn start_game(tag: &str) -> Fixture {
    let prefix = std::env::temp_dir()
        .join(format!("uw-inject-chain-{tag}-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&prefix).unwrap();
    let game = Command::new(wine())
        .arg(win_artifact("uw-test-game.exe"))
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .spawn()
        .unwrap();
    let start = Instant::now();
    let game_pid = loop {
        if let Some(p) = game_pid_in(&prefix) {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(180), "uw-test-game.exe never started");
        std::thread::sleep(Duration::from_millis(500));
    };
    Fixture { prefix, game, game_pid }
}

/// Files starting with `stem` in the prefix users' `%TEMP%`
/// (`drive_c/users/<user>/AppData/Local/Temp`). **No recursion:** `dosdevices/z: -> /` means a
/// recursive walk would follow that symlink out into the whole filesystem and never finish.
fn markers(prefix: &Path, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for user in std::fs::read_dir(prefix.join("drive_c/users")).into_iter().flatten().flatten() {
        let temp = user.path().join("AppData/Local/Temp");
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(stem) {
                out.push(e.path());
            }
        }
    }
    out
}

/// Run `uw-inject --proton <wine-root> --exe uw-test-game.exe …` (it finds the running game by
/// exe name), collect its printed events until one contains `until` (or `within` passes), then
/// end it with SIGTERM — which, by design, also ends its helper. Events are read on a thread so
/// the deadline always holds.
fn run_inject(fx: &Fixture, until: &str, within: Duration) -> Vec<String> {
    // `--proton <root>` is any Wine install with `bin/wine`; the system Wine's root is /usr.
    let proton_root = Path::new(&wine()).parent().unwrap().parent().unwrap().to_path_buf();
    let mut child = Command::new(env!("CARGO_BIN_EXE_uw-inject"))
        .args(["--exe", "uw-test-game.exe", "--pid"])
        .arg(fx.game_pid.to_string())
        .arg("--proton")
        .arg(&proton_root)
        .arg("--helper")
        .arg(win_artifact("uw-helper64.exe"))
        .arg("--bootstrap")
        .arg(win_artifact("uw_bootstrap64.dll"))
        .arg("--overlay")
        .arg(win_artifact("uw_test_overlay.dll"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + within;
    let mut events = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(event) => {
                let done = event.contains(until);
                events.push(event);
                if done {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    // SAFETY: signals our own child process.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    wait_bounded(&mut child, Duration::from_secs(10));
    events
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn uw_inject_connects_and_loads_the_overlay() {
    let fx = start_game("load");
    let events = run_inject(&fx, r#""code":"ok""#, Duration::from_secs(200));
    assert!(events.iter().any(|e| e.contains("bootstrap_connected")), "events: {events:?}");
    assert!(events.iter().any(|e| e.contains(r#""code":"ok""#)), "events: {events:?}");
    assert_eq!(markers(&fx.prefix, "uw-test-overlay-").len(), 1, "the overlay marker should exist in the prefix");

    // A second uw-inject run (client restart) reconnects without reloading the overlay.
    let events2 = run_inject(&fx, "bootstrap_connected", Duration::from_secs(60));
    assert!(
        events2.iter().any(|e| e.contains("bootstrap_connected") && e.contains(r#""overlay_loaded":true"#)),
        "events2: {events2:?}"
    );
    assert_eq!(markers(&fx.prefix, "uw-test-overlay-").len(), 1, "the overlay must not load twice");
}
