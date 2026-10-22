#![cfg(unix)]
//! End-to-end proof of the hook mechanism under plain Wine (system `/usr/bin/wine`, not Proton).
//! A `uw-test-game` window runs in a scratch prefix; `uw-helper64.exe` runs in the same prefix
//! (same wineserver), hooks the game's UI thread, and the bootstrap writes its proof file.
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-helper
//!   xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE` overrides the `wine` binary (default `/usr/bin/wine`); `UW_WINESERVER` the
//! `wineserver` (default `/usr/bin/wineserver`). The scratch prefix goes under the system temp.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn hook_loads_bootstrap_into_the_game() {
    let mut pfx = setup("proof");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());

    // The first run creates the prefix (wineboot); the helper polls for the game window. Give the
    // whole proof up to 200 s.
    let out = wine_cmd(&pfx, &win_artifact("uw-helper64.exe"))
        .arg("uw-test-game.exe")
        .arg("--bootstrap")
        .arg(to_wine_path(&win_artifact("uw_bootstrap64.dll")))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output_timeout(Duration::from_secs(200));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("proof "), "helper did not prove the hook; stdout: {stdout}");
    // The bootstrap wrote its proof file into the prefix: its presence is the proof.
    assert_eq!(markers(&pfx, "uw-bootstrap-proof-").len(), 1, "expected one proof file in the prefix");
}

/// Minimal `wait_with_output` with a timeout, so a stuck Wine process fails the test (and is
/// killed) instead of hanging it.
trait WaitTimeout {
    fn wait_with_output_timeout(self, d: Duration) -> std::process::Output;
}
impl WaitTimeout for Child {
    fn wait_with_output_timeout(mut self, d: Duration) -> std::process::Output {
        let start = Instant::now();
        loop {
            if let Some(_status) = self.try_wait().unwrap() {
                return self.wait_with_output().unwrap();
            }
            if start.elapsed() >= d {
                let _ = self.kill();
                let _ = self.wait();
                panic!("helper did not exit within {d:?}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
