use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

use uw_inject::driver::Driver;
use uw_inject::{exe_matches, to_wine_path};
use uw_pipe_proto::jsonl::{Command, Event};

#[cfg(unix)]
use uw_inject::discovery::{
    cmdline_names, config_info_for_prefix, find_pids, proton_files_from_config_info, server_socket, wine_env,
    GameError, GameSession,
};

// First lines of a real `compatdata/1422450/config_info` (Proton 11.0).
#[cfg(unix)]
const CONFIG_INFO: &str = "11.0-100
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/share/fonts/
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/lib/
/home/buga/.local/share/Steam
1791921018.0
";

#[cfg(unix)]
#[test]
fn proton_files_dir_comes_from_config_info() {
    assert_eq!(
        proton_files_from_config_info(CONFIG_INFO),
        Some(PathBuf::from("/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files"))
    );
    assert_eq!(proton_files_from_config_info("11.0-100\n/home/buga\n"), None);
}

#[cfg(unix)]
#[test]
fn config_info_sits_next_to_the_prefix() {
    assert_eq!(
        config_info_for_prefix(Path::new("/s/compatdata/1422450/pfx/")),
        PathBuf::from("/s/compatdata/1422450/config_info")
    );
}

#[cfg(unix)]
#[test]
fn wine_env_keeps_prefix_and_sync_settings_only() {
    let environ = b"PATH=/usr/bin\0WINEPREFIX=/s/compatdata/1422450/pfx/\0WINEFSYNC=1\0\
WINEESYNC=0\0WINENTSYNC=1\0WINEDLLPATH=/run/x\0LD_PRELOAD=/a.so\0";
    assert_eq!(
        wine_env(environ),
        vec![
            ("WINEPREFIX".to_string(), "/s/compatdata/1422450/pfx/".to_string()),
            ("WINEFSYNC".to_string(), "1".to_string()),
            ("WINEESYNC".to_string(), "0".to_string()),
            ("WINENTSYNC".to_string(), "1".to_string()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn server_socket_matches_wine_layout() {
    assert_eq!(
        server_socket(1000, 0x24, 0x17e2e),
        PathBuf::from("/tmp/.wine-1000/server-24-17e2e/socket")
    );
}

#[cfg(unix)]
#[test]
fn game_session_find_reports_a_missing_process() {
    let err = GameSession::find("uw-no-such-game.exe", None).unwrap_err();
    assert_eq!(err.code(), "game_not_found");
    assert!(matches!(err, GameError::NotRunning(_)));
    assert!(err.to_string().contains("uw-no-such-game.exe is not running"), "{err}");
}

#[cfg(unix)]
#[test]
fn game_error_codes_match_the_design() {
    assert_eq!(GameError::NotRunning("x".into()).code(), "game_not_found");
    assert_eq!(GameError::NoWineSession("x".into()).code(), "no_wine_session");
}

#[cfg(unix)]
#[test]
fn find_pids_sees_this_test_process() {
    let argv0 = std::env::args().next().unwrap();
    let base = argv0.rsplit('/').next().unwrap().to_string();
    assert!(find_pids(&base).contains(&(std::process::id() as i32)));
}

#[test]
fn unix_paths_map_to_wine_z_drive() {
    assert_eq!(
        to_wine_path(Path::new("/home/buga/.local/share/underwolf/a.dll")),
        r"Z:\home\buga\.local\share\underwolf\a.dll"
    );
}

#[test]
fn exe_matches_basename_case_insensitively() {
    assert!(exe_matches("Deadlock.EXE", "deadlock.exe"));
    assert!(exe_matches(r"S:\common\Deadlock\game\bin\win64\deadlock.exe", "deadlock.exe"));
    assert!(!exe_matches("deadlock.exe.bak", "deadlock.exe"));
}

#[test]
fn driver_loads_overlay_once_when_bootstrap_connects_fresh() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Fresh connection with no overlay: send load_overlay once.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }),
        vec![Command::LoadOverlay { request_id: 1, path: r"Z:\games\overlay.dll".into() }]
    );
    // A later status does not re-send.
    assert_eq!(
        d.on_event(&Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() }),
        vec![]
    );
    assert!(d.is_done());
}

#[test]
fn driver_does_not_reload_after_client_restart() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Reconnect: the pinned bootstrap already has the overlay, so send nothing.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\games\overlay.dll".into()) }),
        vec![]
    );
    assert!(d.is_done());
}

#[test]
fn driver_with_no_overlay_sends_nothing() {
    let mut d = Driver::new(None);
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }), vec![]);
}

#[cfg(unix)]
#[test]
fn live_cmdlines_game_matches_and_proton_wrapper_does_not() {
    // Real NUL-separated /proc/<pid>/cmdline bytes. The game's comm is "MainThrd".
    let game = b"S:\\steamapps\\common\\Deadlock\\game\\bin\\win64\\deadlock.exe\0";
    // The Proton wrapper: steam.exe is argv[0], the Linux game path only argv[1].
    let wrapper = b"c:\\windows\\system32\\steam.exe\0/home/buga/.local/share/Steam/steamapps/common/Deadlock/game/bin/win64/deadlock.exe\0";
    assert!(cmdline_names(game, "deadlock.exe"));
    assert!(!cmdline_names(wrapper, "deadlock.exe"));
}

#[test]
fn wine_paths_keep_spaces_and_unicode_and_absolutize() {
    assert_eq!(
        to_wine_path(Path::new("/home/u/My Games/ünï 日本/a b.dll")),
        r"Z:\home\u\My Games\ünï 日本\a b.dll"
    );
    let rel = to_wine_path(Path::new("rel/x.dll"));
    assert!(rel.starts_with(r"Z:\") && rel.ends_with(r"\rel\x.dll"), "{rel}");
}

#[test]
fn exe_matches_rejects_suffix_only() {
    assert!(!exe_matches("notdeadlock.exe", "deadlock.exe"));
}

#[cfg(unix)]
#[test]
fn wine_env_drops_non_sync_wine_vars() {
    let environ = b"WINEDLLOVERRIDES=steam.exe=b\0WINEPREFIX=/p\0";
    assert_eq!(wine_env(environ), vec![("WINEPREFIX".to_string(), "/p".to_string())]);
}
