#![cfg(unix)]

use std::ffi::OsStr;
use std::path::Path;

use uw_spike_inject::launcher::{check_wine_session, find_pids, wine_command};

#[test]
fn wine_command_runs_injector_with_proton_wine_and_game_env() {
    let env = vec![
        ("WINEPREFIX".to_string(), "/s/pfx/".to_string()),
        ("WINEFSYNC".to_string(), "1".to_string()),
    ];
    let cmd = wine_command(
        Path::new("/p/Proton 11.0/files"),
        &env,
        Path::new("/home/u/.local/share/underwolf-spike/uw-spike-inject-win.exe"),
        "deadlock.exe",
        Path::new("/home/u/.local/share/underwolf-spike/uw_spike_input.dll"),
    );
    assert_eq!(cmd.get_program(), OsStr::new("/p/Proton 11.0/files/bin/wine"));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(
        args,
        [
            OsStr::new("/home/u/.local/share/underwolf-spike/uw-spike-inject-win.exe"),
            OsStr::new("deadlock.exe"),
            OsStr::new(r"Z:\home\u\.local\share\underwolf-spike\uw_spike_input.dll"),
        ]
    );
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("WINEPREFIX"), Some(OsStr::new("/s/pfx/")))));
    assert!(envs.contains(&(OsStr::new("WINEFSYNC"), Some(OsStr::new("1")))));
    // Our own shell's Wine settings must not leak in and point at another session.
    assert!(envs.contains(&(OsStr::new("WINESERVER"), None)));
}

#[test]
fn no_wine_session_for_a_prefix_without_a_running_server() {
    let dir = std::env::temp_dir().join(format!("uw-inject-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let err = check_wine_session(&dir).unwrap_err();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(err.contains("no running Wine session"), "{err}");
}

#[test]
fn find_pids_sees_this_test_process() {
    let argv0 = std::env::args().next().unwrap();
    let base = argv0.rsplit('/').next().unwrap().to_string();
    assert!(find_pids(&base).contains(&(std::process::id() as i32)));
}
