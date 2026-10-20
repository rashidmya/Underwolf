#![cfg(unix)]
//! End-to-end injection under Proton's own Wine, without the game: a sleeping Windows exe runs in
//! a scratch prefix, `uw-spike-inject` joins its Wine session from outside and injects the real C3
//! DLL, and the DLL proves it loaded by greeting us over TCP.
//!
//! Needs Proton and the Windows artifacts, so it is ignored by default:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-input -p uw-spike-inject
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-inject --examples
//!   cargo test -p uw-spike-inject --test wine_e2e -- --ignored
//! `UW_PROTON_FILES` overrides the Proton `files` directory (default: Proton 11.0 in the Steam library).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use uw_spike_inject::launcher::find_pids;
use uw_spike_ipc::{Msg, read_msg};

fn proton_files() -> PathBuf {
    std::env::var_os("UW_PROTON_FILES").map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").unwrap();
        Path::new(&home).join(".local/share/Steam/steamapps/common/Proton 11.0/files")
    })
}

fn win_artifact(rel: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/x86_64-pc-windows-gnu/release");
    let p = root.join(rel);
    assert!(p.exists(), "missing {} (build the Windows artifacts first)", p.display());
    p
}

struct Session {
    sleeper: Child,
    prefix: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.sleeper.kill();
        let _ = Command::new(proton_files().join("bin/wineserver"))
            .arg("-k")
            .env("WINEPREFIX", &self.prefix)
            .status();
    }
}

#[test]
#[ignore = "needs Proton and the x86_64-pc-windows-gnu artifacts"]
fn injects_c3_dll_into_a_running_wine_process() {
    let prefix = PathBuf::from("/tmp/opencode/uw-inject-e2e-pfx");
    std::fs::create_dir_all(&prefix).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let sleeper = Command::new(proton_files().join("bin/wine"))
        .arg(win_artifact("examples/wine_sleeper.exe"))
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("UW_SPIKE_HOST_PORT", port.to_string())
        .spawn()
        .unwrap();
    let _session = Session { sleeper, prefix: prefix.clone() };

    // The first run creates the prefix (wineboot), which takes a while.
    let start = Instant::now();
    while find_pids("wine_sleeper.exe").is_empty() {
        assert!(start.elapsed() < Duration::from_secs(180), "wine_sleeper.exe never started");
        std::thread::sleep(Duration::from_millis(500));
    }

    let status = Command::new(env!("CARGO_BIN_EXE_uw-spike-inject"))
        .args(["--exe", "wine_sleeper.exe", "--proton"])
        .arg(proton_files())
        .arg("--injector")
        .arg(win_artifact("uw-spike-inject-win.exe"))
        .arg("--dll")
        .arg(win_artifact("uw_spike_input.dll"))
        .status()
        .unwrap();
    assert!(status.success(), "uw-spike-inject failed: {status}");

    listener.set_nonblocking(true).unwrap();
    let start = Instant::now();
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(conn) => break conn,
            Err(_) if start.elapsed() < Duration::from_secs(15) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("injected DLL never connected: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    match read_msg(&mut stream).unwrap() {
        Msg::Hello { .. } => {}
        other => panic!("expected Hello from the injected DLL, got {other:?}"),
    }
}
