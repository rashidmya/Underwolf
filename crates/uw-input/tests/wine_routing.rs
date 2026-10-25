#![cfg(unix)]
use std::{path::PathBuf, process::Command};
fn probe(case: &str) {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release/examples/input_probe.exe")
        .canonicalize()
        .expect("build input_probe first");
    let prefix = std::env::temp_dir().join(format!("uw-routing-{}-{case}", std::process::id()));
    std::fs::create_dir_all(&prefix).unwrap();
    let result = Command::new("timeout")
        .args(["60", "wine"])
        .arg(binary)
        .args(["--routing", case])
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .output()
        .unwrap();
    let _ = Command::new("wineserver").arg("-k").env("WINEPREFIX", &prefix).status();
    let _ = std::fs::remove_dir_all(prefix);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("PASS routing probe"));
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn mouse_only_keeps_keyboard_original() {
    probe("mouse-only");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn keyboard_only_keeps_pointer_original() {
    probe("keyboard-only");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn exclusive_owns_both_domains() {
    probe("exclusive");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn notification_preserves_original_input() {
    probe("notify");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn optional_runtime_load_revokes_before_off_path_discovery() {
    probe("optional-load");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn failed_native_entry_rolls_back_without_application_success() {
    probe("rollback");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn raw_mouse_only_keeps_keyboard_original() {
    probe("raw-mouse-only");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn raw_notification_preserves_original_input() {
    probe("raw-notify");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn raw_and_legacy_cycles_have_one_named_activation() {
    probe("raw-block");
}
