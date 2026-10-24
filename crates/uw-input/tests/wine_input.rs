//! Real Windows API probe in an isolated Wine prefix. Native assertions live in input_probe.
#![cfg(unix)]
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
struct Prefix(PathBuf);
impl Drop for Prefix {
    fn drop(&mut self) {
        let _ = Command::new("wineserver").arg("-k").env("WINEPREFIX", &self.0).status();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn probe(args: &[&str]) {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release/examples/input_probe.exe")
        .canonicalize()
        .expect("build input_probe first");
    let prefix = Prefix(std::env::temp_dir().join(format!(
        "uw-input-probe-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    )));
    std::fs::create_dir_all(&prefix.0).unwrap();
    let output = Command::new("timeout")
        .args(["60", "wine"])
        .arg(binary)
        .args(args)
        .env("WINEPREFIX", &prefix.0)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "probe {args:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS input probe"));
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn raw_registration_hotkey_enters_and_lease_releases() {
    probe(&["--hotkey"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn cursor_registration_and_stalled_ui_restore() {
    probe(&["--cursor", "--stall-ui"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn buffered_raw_hotkey_and_suppression() {
    probe(&["--buffered", "--hotkey"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn control_overflow_disconnects() {
    probe(&["--overflow"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn destruction_does_not_restore_old_clip() {
    probe(&["--cursor", "--destroy"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn five_buttons_wheels_unicode_and_held_key_polling() {
    probe(&["--mouse", "--unicode"]);
}
