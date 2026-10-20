use uw_spike_common::target::is_target_cmdline;

#[test]
fn matches_windows_paths_case_insensitively() {
    assert!(is_target_cmdline(
        r"C:\Program Files (x86)\Steam\steamapps\common\Deadlock\game\bin\win64\deadlock.exe"
    ));
    assert!(is_target_cmdline(
        r"Z:\home\u\.local\share\Steam\steamapps\common\Deadlock\game\bin\win64\DEADLOCK.EXE"
    ));
}

#[test]
fn rejects_other_processes() {
    assert!(!is_target_cmdline("/opt/electron/electron"));
    assert!(!is_target_cmdline(r"C:\x\vconsole2.exe"));
    assert!(!is_target_cmdline("deadlock.exe.bak"));
}
