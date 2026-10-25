#![cfg(unix)]
#[path = "support/wine.rs"]
mod fixture;
use fixture::probe;
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
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn continuing_mouse_domain_stays_blocked_during_keyboard_gain() {
    probe("partial");
}

#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn host_focus_change_resets_pending_utf16_text() {
    probe("focus-text");
}
