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

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn held_key_remains_quarantined_after_exit() {
    probe(&["--held"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn startup_ctrl_release_does_not_leave_a_phantom_modifier() {
    probe(&["--startup-ctrl"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn replacement_window_requires_explicit_recapture() {
    probe(&["--replace"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn idle_expired_lease_rejects_entry() {
    probe(&["--expired-entry"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn actual_focus_transfer_releases_without_recapture() {
    probe(&["--focus", "--cursor"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn hide_revokes_before_stalled_ui_can_restore() {
    probe(&["--hide-stall", "--visibility-only"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn combined_hide_revokes_without_invalidating_its_own_request() {
    probe(&["--hide-stall"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn replacement_preserves_pending_owner_restoration() {
    probe(&["--replace-pending"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn reentrant_game_registration_cancels_entry() {
    probe(&["--reentrant-register"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn reentrant_cursor_changes_cannot_activate_capture() {
    probe(&["--reentrant-cursor"]);
}
#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn cleanup_callback_focus_change_prevents_synthetic_injection() {
    probe(&["--release-focus"]);
}

#[test]
#[ignore = "build Windows input_probe; run under xvfb-run"]
fn double_clicks_are_captured_as_button_edges() {
    probe(&["--mouse", "--double-click"]);
}
