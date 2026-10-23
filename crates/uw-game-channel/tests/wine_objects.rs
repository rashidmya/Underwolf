#![cfg(unix)]
//! The plan 2a gate under plain Wine (system `/usr/bin/wine`, not Proton): two Wine processes in
//! one scratch prefix share the channel objects. `game_probe.exe` opens them as the in-game worker
//! will, `helper_probe.exe` creates them as the helper will; between them they prove the named
//! events, the mask region's lock-free atomics (with a lost-update check), the state block, the
//! one-guard rule, and an abandoned lock (taken as acquired, header re-checked) across processes.
//! See the two examples for what each checks.
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
//!   timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}

fn example(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release/examples")
        .join(name);
    assert!(p.exists(), "missing {} (build the examples first)", p.display());
    p
}

/// Wait up to `d` for `child` to exit, then kill it; the exit status if it exited by itself.
fn wait_bounded(child: &mut Child, d: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// The scratch prefix and both probes. Dropping it (on success or panic) kills the probes, ends
/// this prefix's wineserver only, and removes the prefix.
struct Fixture {
    dir: PathBuf,
    game: Option<Child>,
    helper: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for c in [self.helper.take(), self.game.take()].into_iter().flatten() {
            let mut c = c;
            let _ = c.kill();
            wait_bounded(&mut c, Duration::from_secs(10));
        }
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.dir).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap_or(&self.dir));
    }
}

fn wine_cmd(dir: &Path, program: &Path) -> Command {
    let mut c = Command::new(wine());
    c.arg(program)
        .env("WINEPREFIX", dir)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn drain(child: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut s);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut s);
    }
    s
}

/// Catches the design's open risk: events, the mask atomics or the state block not shared
/// between two Wine processes, a doorbell lost, or a bit lost to a non-atomic OR or swap. Also
/// catches a second guard on one thread, and an abandoned lock that is not taken as acquired or
/// whose header is not re-checked (design "Failures").
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu examples"]
fn the_channel_objects_work_between_two_wine_processes() {
    let dir = std::env::temp_dir().join(format!("uw-inject-gate-{}", std::process::id())).join("pfx");
    std::fs::create_dir_all(&dir).unwrap();
    let mut fx = Fixture { dir, game: None, helper: None };

    fx.game = Some(wine_cmd(&fx.dir, &example("game_probe.exe")).spawn().unwrap());
    // Its first line is `pid <n>`. The first run boots the prefix, so allow up to 200 s.
    let stdout = fx.game.as_mut().unwrap().stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next().and_then(Result::ok).unwrap_or_default();
        let _ = tx.send(first);
        // Keep the rest (the final `ok`) for the assertion below.
        let rest: Vec<String> = lines.map_while(Result::ok).collect();
        let _ = tx.send(rest.join("\n"));
    });
    let first = rx.recv_timeout(Duration::from_secs(200)).expect("game_probe printed no pid");
    let pid = first.strip_prefix("pid ").unwrap_or_else(|| panic!("unexpected first line {first:?}")).to_string();

    fx.helper = Some(wine_cmd(&fx.dir, &example("helper_probe.exe")).arg(&pid).spawn().unwrap());
    let helper_status = wait_bounded(fx.helper.as_mut().unwrap(), Duration::from_secs(120));
    let helper_out = drain(fx.helper.as_mut().unwrap());
    let game_status = wait_bounded(fx.game.as_mut().unwrap(), Duration::from_secs(30));
    let game_rest = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let game_err = drain(fx.game.as_mut().unwrap());

    assert!(
        helper_status.is_some_and(|s| s.success()) && helper_out.contains("ok rounds=200 sends=[2000, 2000]"),
        "helper_probe: {helper_status:?}\n{helper_out}\ngame_probe: {game_status:?}\n{game_rest}\n{game_err}"
    );
    assert!(
        game_status.is_some_and(|s| s.success()) && game_rest.contains("ok"),
        "game_probe: {game_status:?}\n{game_rest}\n{game_err}\nhelper_probe:\n{helper_out}"
    );
}
