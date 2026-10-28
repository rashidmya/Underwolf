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
