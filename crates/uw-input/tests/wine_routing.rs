#![cfg(unix)]
#[path = "support/wine.rs"]
mod fixture;
use fixture::probe;
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn focus_return_reconciles_released_and_still_held_modifiers() {
    probe("focus-return");
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

#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn queued_edit_request_uses_the_latest_observed_state_sequence() {
    probe("sequence");
}

#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn reentrant_newer_state_prevents_stale_native_publication() {
    probe("sequence-reentrant");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn legacy_state_cannot_announce_a_routing_delivery_epoch() {
    probe("first-routing-state");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn native_coverage_follows_its_routing_epoch_state() {
    probe("optional-order");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn raw_target_on_another_thread_keeps_hotkeys_and_widget_capture() {
    probe("raw-thread");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn native_client_resize_cancels_and_rejects_an_invisible_takeover() {
    probe("resize");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn raw_worker_notification_preserves_game_input_and_delivers_text_once() {
    probe("raw-thread-notify");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run"]
fn concurrent_worker_polling_preserves_notification_focus_and_named_edges() {
    probe("raw-thread-poll-notify");
}
