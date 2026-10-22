// The crate's library name is `uw_bootstrap64` (the `[lib] name`), so the test imports that.
use uw_bootstrap64::state::{Decision, OverlayState};

#[test]
fn fresh_state_loads() {
    let s = OverlayState::new();
    assert_eq!(s.decide(r"Z:\g\overlay.dll"), Decision::Load);
    assert_eq!(s.loaded_path(), None);
}

#[test]
fn same_path_is_already_loaded_case_insensitively() {
    let mut s = OverlayState::new();
    s.mark_loaded(r"Z:\Games\Overlay.dll".into());
    assert_eq!(s.decide(r"z:\games\overlay.dll"), Decision::AlreadyLoaded);
    assert_eq!(s.loaded_path(), Some(r"Z:\Games\Overlay.dll"));
}

#[test]
fn a_different_path_is_already_loaded_other() {
    let mut s = OverlayState::new();
    s.mark_loaded(r"Z:\g\overlay.dll".into());
    assert_eq!(s.decide(r"Z:\g\other.dll"), Decision::AlreadyLoadedOther);
}
