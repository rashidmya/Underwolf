use uw_vk_layer::*;
#[test]
fn only_target_basename_or_explicitly_scoped_probe_can_opt_in() {
    for name in ["deadlock.exe", "/steam/DEADLOCK.EXE", "deadlock"] {
        assert!(target_process(name, false));
    }
    assert!(target_process("/tmp/uw-render-probe", true));
    for name in [
        "/deadlock.exe/other",
        "other.exe",
        "uw-render-probe.exe",
        "uw-render-probe",
    ] {
        assert!(!target_process(name, false));
    }
    assert!(!target_process("other.exe", true));
}

#[test]
fn proton_uses_the_game_argv0_and_probe_opt_in_requires_the_real_executable() {
    assert!(target_identity(
        "/usr/bin/wine64-preloader",
        b"Z:\\steam\\DEADLOCK.EXE\0--flag\0",
        false
    ));
    assert!(!target_identity(
        "/usr/bin/other",
        b"other\0deadlock.exe\0",
        true
    ));
    assert!(!target_identity(
        "/usr/bin/other",
        b"uw-render-probe\0",
        true
    ));
    assert!(target_identity(
        "/tmp/uw-render-probe",
        b"uw-render-probe\0",
        true
    ));
    assert!(!target_identity(
        "/tmp/uw-render-probe",
        b"uw-render-probe\0",
        false
    ));
}
