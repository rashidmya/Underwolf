#![cfg(unix)]
//! End-to-end injection under Proton's own Wine, without the game: a sleeping Windows exe runs in
//! a scratch prefix, `uw-spike-inject` joins its Wine session from outside and injects the real C3
//! DLL, and the DLL proves it loaded by greeting us over TCP.
//!
//! A second test runs Spike 3's chain the same way: `uw-gep` injects the module, which loads the
//! collector plugin in `selftest=1` mode; the Wine-side helper relays its channel to `uw-gep`.
//!
//! Needs Proton and the Windows artifacts, so it is ignored by default:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-input -p uw-spike-inject \
//!       -p uw-spike-dlcollector -p uw-spike-gep-helper
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-inject --examples
//!   cargo build -p uw-spike-gep-host
//!   cargo test -p uw-spike-inject --test wine_e2e -- --ignored
//! `UW_PROTON_FILES` overrides the Proton `files` directory (default: Proton 11.0 in the Steam
//! library; any Wine install with `bin/wine` and `bin/wineserver` works, e.g. `/usr`).
//! `UW_GEP_BIN` overrides the `uw-gep` binary (default: `target/debug/uw-gep`).
//! The scratch prefixes and the JSONL log go under `std::env::temp_dir()` (`$TMPDIR` or `/tmp`).

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
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

/// Both tests find their target by exe name, so they must not run at the same time.
static SERIAL: Mutex<()> = Mutex::new(());

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
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let prefix = std::env::temp_dir().join("uw-inject-e2e-pfx");
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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_sleeper(prefix: &Path, host_port: u16) -> Session {
    std::fs::create_dir_all(prefix).unwrap();
    let sleeper = Command::new(proton_files().join("bin/wine"))
        .arg(win_artifact("examples/wine_sleeper.exe"))
        .env("WINEPREFIX", prefix)
        .env("WINEDEBUG", "-all")
        .env("UW_SPIKE_HOST_PORT", host_port.to_string())
        .spawn()
        .unwrap();
    let session = Session { sleeper, prefix: prefix.to_path_buf() };
    let start = Instant::now();
    while find_pids("wine_sleeper.exe").is_empty() {
        assert!(start.elapsed() < Duration::from_secs(180), "wine_sleeper.exe never started");
        std::thread::sleep(Duration::from_millis(500));
    }
    session
}

/// Stops `uw-gep` (SIGINT, so it prints its table and stops its helper) if the test bails out.
struct Gep(Child);

impl Drop for Gep {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGINT);
        }
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn jsonl_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(String::from).collect()
}

fn count(lines: &[String], needles: &[&str]) -> usize {
    lines.iter().filter(|l| needles.iter().all(|n| l.contains(n))).count()
}

/// Waits until `done` holds for the JSONL lines.
fn wait_for(path: &Path, what: &str, secs: u64, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let start = Instant::now();
    loop {
        let lines = jsonl_lines(path);
        if done(&lines) {
            return lines;
        }
        assert!(start.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}; JSONL:\n{}", lines.join("\n"));
        std::thread::sleep(Duration::from_millis(200));
    }
}

const SELFTEST_STATUS: [&str; 3] = [r#""kind":"info""#, r#""key":"plugin_status""#, r#""value":"selftest""#];
const SELFTEST_EVENT: [&str; 2] = [r#""kind":"event""#, r#""name":"selftest""#];
const CONNECTED: [&str; 2] = [r#""kind":"status""#, r#""status":"connected""#];

#[test]
#[ignore = "needs Proton (or UW_PROTON_FILES), the x86_64-pc-windows-gnu artifacts and uw-gep"]
fn uw_gep_receives_collector_selftest_and_snapshot_after_helper_restart() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let gep_bin = std::env::var_os("UW_GEP_BIN").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/uw-gep")
    });
    assert!(gep_bin.exists(), "missing {} (cargo build -p uw-spike-gep-host)", gep_bin.display());
    let win_dir = win_artifact("uw-gep-helper.exe").parent().unwrap().to_path_buf();
    for f in ["uw_spike_input.dll", "uw_spike_dlcollector.dll", "uw-spike-inject-win.exe"] {
        win_artifact(f);
    }

    let (host_port, gep_port) = (free_port(), free_port());
    let _session = start_sleeper(&std::env::temp_dir().join("uw-gep-e2e-pfx"), host_port);
    let jsonl = std::env::temp_dir().join("uw-gep-e2e.jsonl");
    let _ = std::fs::remove_file(&jsonl);

    let child = Command::new(&gep_bin)
        .args(["watch", "--channel", "7", "--args", "selftest=1", "--exe", "wine_sleeper.exe", "--jsonl"])
        .arg(&jsonl)
        .arg("--proton")
        .arg(proton_files())
        .arg("--install-dir")
        .arg(&win_dir)
        .env("UW_SPIKE_HOST_PORT", host_port.to_string())
        .env("UW_SPIKE_GEP_PORT", gep_port.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut gep = Gep(child);

    let lines = wait_for(&jsonl, "the selftest status and event", 90, |l| {
        count(l, &SELFTEST_STATUS) >= 1 && count(l, &SELFTEST_EVENT) >= 1
    });
    assert_eq!(count(&lines, &CONNECTED), 1);
    assert_eq!(count(&lines, &[r#""status":"plugin_loaded""#, r#""ok":true"#]), 1);

    // Kill the helper: uw-gep restarts it, and the new one sends the whole snapshot again.
    let helpers = find_pids("uw-gep-helper.exe");
    assert_eq!(helpers.len(), 1, "expected one helper, found {helpers:?}");
    unsafe {
        libc::kill(helpers[0], libc::SIGKILL);
    }
    let lines = wait_for(&jsonl, "a second snapshot from a restarted helper", 60, |l| {
        count(l, &CONNECTED) >= 2 && count(l, &SELFTEST_STATUS) >= 2
    });
    // The restarted helper replays the ring; uw-gep counts each event once.
    assert_eq!(count(&lines, &SELFTEST_EVENT), 1);
    let new_helpers = find_pids("uw-gep-helper.exe");
    assert!(new_helpers.len() == 1 && new_helpers[0] != helpers[0], "helpers now: {new_helpers:?}");

    // Ctrl+C: uw-gep stops its helper and prints the status table.
    unsafe {
        libc::kill(gep.0.id() as i32, libc::SIGINT);
    }
    let mut table = String::new();
    for line in BufReader::new(gep.0.stdout.take().unwrap()).lines() {
        table.push_str(&line.unwrap());
        table.push('\n');
    }
    assert!(gep.0.wait().unwrap().success());
    println!("{table}");
    assert!(table.contains("plugin_status: selftest"), "{table}");
    assert!(table.contains("match_clock"), "{table}");
    let start = Instant::now();
    while !find_pids("uw-gep-helper.exe").is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10), "uw-gep left its helper running");
        std::thread::sleep(Duration::from_millis(200));
    }
}
