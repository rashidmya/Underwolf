#![cfg(unix)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use uw_inject::discovery::{helper_command, GameSession};

fn session() -> GameSession {
    GameSession {
        pid: 42,
        env: vec![
            ("WINEPREFIX".to_string(), "/s/compatdata/1/pfx".to_string()),
            ("WINEFSYNC".to_string(), "1".to_string()),
        ],
        prefix: PathBuf::from("/s/compatdata/1/pfx"),
        proton: PathBuf::from("/p/Proton 11.0/files"),
    }
}

#[test]
fn helper_command_runs_helper_in_the_game_session() {
    let cmd = helper_command(
        &session(),
        Path::new("/home/u/.local/share/underwolf/uw-helper64.exe"),
        "deadlock.exe",
        Path::new("/home/u/.local/share/underwolf/uw_bootstrap64.dll"),
        false,
    );
    assert_eq!(cmd.get_program(), OsStr::new("/p/Proton 11.0/files/bin/wine"));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(
        args,
        [
            OsStr::new("/home/u/.local/share/underwolf/uw-helper64.exe"),
            OsStr::new("deadlock.exe"),
            OsStr::new("--bootstrap"),
            OsStr::new(r"Z:\home\u\.local\share\underwolf\uw_bootstrap64.dll"),
        ]
    );
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("WINEPREFIX"), Some(OsStr::new("/s/compatdata/1/pfx")))));
    assert!(envs.contains(&(OsStr::new("WINEFSYNC"), Some(OsStr::new("1")))));
    // Our own shell's Wine settings must not leak in and point at another session.
    assert!(envs.contains(&(OsStr::new("WINESERVER"), None)));
}

#[test]
fn helper_command_passes_fallback_flag() {
    let cmd = helper_command(
        &session(),
        Path::new("/x/uw-helper64.exe"),
        "deadlock.exe",
        Path::new("/x/uw_bootstrap64.dll"),
        true,
    );
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(args.last(), Some(&OsStr::new("--fallback")));
}
