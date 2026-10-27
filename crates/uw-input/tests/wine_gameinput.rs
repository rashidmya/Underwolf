#![cfg(unix)]
#[path = "support/wine.rs"]
mod fixture;

#[test]
#[ignore = "Official runtime in UW_TEST_GAMEINPUT_RUNTIME; input_probe under xvfb-run in a fresh prefix"]
fn late_gameinput_redist_load_revokes_before_discovery() {
    fixture::probe("optional-redist");
}
