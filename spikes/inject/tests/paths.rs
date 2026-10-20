use std::path::{Path, PathBuf};

use uw_spike_inject::{
    config_info_for_prefix, exe_matches, proton_files_from_config_info, remote_proc_address,
    server_socket, to_wine_path, wine_env,
};

// First lines of a real `compatdata/1422450/config_info` (Proton 11.0).
const CONFIG_INFO: &str = "11.0-100
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/share/fonts/
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/lib/
/home/buga/.local/share/Steam
1791921018.0
";

#[test]
fn proton_files_dir_comes_from_config_info() {
    assert_eq!(
        proton_files_from_config_info(CONFIG_INFO),
        Some(PathBuf::from("/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files"))
    );
}

#[test]
fn proton_files_dir_is_none_without_a_files_path() {
    assert_eq!(proton_files_from_config_info("11.0-100\n/home/buga\n"), None);
}

#[test]
fn config_info_sits_next_to_the_prefix() {
    assert_eq!(
        config_info_for_prefix(Path::new("/s/compatdata/1422450/pfx/")),
        PathBuf::from("/s/compatdata/1422450/config_info")
    );
}

#[test]
fn wine_env_keeps_prefix_and_sync_settings_only() {
    let environ = b"PATH=/usr/bin\0WINEPREFIX=/s/compatdata/1422450/pfx/\0WINEFSYNC=1\0\
WINEESYNC=0\0WINENTSYNC=1\0WINEDLLPATH=/run/x\0LD_PRELOAD=/a.so\0WINEDLLOVERRIDES=steam.exe=b\0";
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

#[test]
fn server_socket_matches_wine_layout() {
    // Deadlock's prefix on the test machine: dev 0x24, inode 0x17e2e.
    assert_eq!(
        server_socket(1000, 0x24, 0x17e2e),
        PathBuf::from("/tmp/.wine-1000/server-24-17e2e/socket")
    );
}

#[test]
fn unix_paths_map_to_wine_z_drive() {
    assert_eq!(
        to_wine_path(Path::new("/home/buga/.local/share/underwolf spike/a.dll")),
        r"Z:\home\buga\.local\share\underwolf spike\a.dll"
    );
}

#[test]
fn exe_matches_basename_case_insensitively() {
    assert!(exe_matches("Deadlock.EXE", "deadlock.exe"));
    assert!(exe_matches(r"S:\steamapps\common\Deadlock\game\bin\win64\deadlock.exe", "deadlock.exe"));
    assert!(!exe_matches("deadlock.exe.bak", "deadlock.exe"));
    assert!(!exe_matches("notdeadlock.exe", "deadlock.exe"));
}

#[test]
fn remote_proc_address_keeps_offset_within_module() {
    assert_eq!(remote_proc_address(0x7ff0_1234, 0x7ff0_0000, 0x6000_0000), 0x6000_1234);
}
