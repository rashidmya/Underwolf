#![cfg(unix)]
#[path = "support/wine.rs"]
mod fixture;
#[test]
#[ignore = "Windows input_probe under xvfb-run, --test-threads=1"]
fn real_8a_8w_custom_formats_preexisting_devices_and_buffered_recovery() {
    fixture::probe("directinput");
}
#[test]
#[ignore = "Windows input_probe under xvfb-run, --test-threads=1"]
fn exclusive_directinput_keeps_the_overlay_hotkey_reachable() {
    fixture::probe("directinput-exclusive");
}
