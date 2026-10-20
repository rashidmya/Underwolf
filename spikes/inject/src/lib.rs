//! Runtime DLL injection into a running game, the Overwolf/Discord way: an outside injector
//! process makes the game call `LoadLibraryW` on our DLL through `CreateRemoteThread`. Nothing
//! in the game folder or the Steam launch options changes.
//!
//! Under Proton the game is a Wine process, so the injector (a Windows PE) must run inside the
//! game's own Wine session to see it. The [`launcher`] does that: it runs the injector with the
//! game's Proton `wine`, prefix and sync settings, which joins the game's running wineserver.

use std::path::{Path, PathBuf};

#[cfg(unix)]
pub mod launcher;

/// The Proton `files` directory (holding `bin/wine`) from a prefix's `config_info`, which lists
/// paths under it such as `.../Proton 11.0/files/lib/`.
pub fn proton_files_from_config_info(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let i = line.find("/files/")?;
        Some(PathBuf::from(&line[..i + "/files".len()]))
    })
}

/// Steam writes `config_info` into `compatdata/<appid>/`, next to the `pfx` prefix directory.
pub fn config_info_for_prefix(prefix: &Path) -> PathBuf {
    let compatdata = prefix.parent().unwrap_or(prefix);
    compatdata.join("config_info")
}

/// The game's Wine settings the injector must share to join its session: the prefix, and the
/// sync mode (fsync/esync/ntsync must match the running wineserver's).
pub fn wine_env(environ: &[u8]) -> Vec<(String, String)> {
    environ
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (key, value) = entry.split_once('=')?;
            let keep = key == "WINEPREFIX" || (key.starts_with("WINE") && key.ends_with("SYNC"));
            keep.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// Where a wineserver for the prefix with this device/inode listens (Wine's `server-<dev>-<ino>`).
pub fn server_socket(uid: u32, dev: u64, ino: u64) -> PathBuf {
    PathBuf::from(format!("/tmp/.wine-{uid}/server-{dev:x}-{ino:x}/socket"))
}

/// A Unix path as Wine's `Z:` drive sees it.
pub fn to_wine_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// True when `name` (a bare exe name or a Windows/Unix path) is the exe `target`.
pub fn exe_matches(name: &str, target: &str) -> bool {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    base.eq_ignore_ascii_case(target)
}

/// The address of a function in another process's copy of a module, from its offset in ours.
pub fn remote_proc_address(local_fn: usize, local_base: usize, remote_base: usize) -> usize {
    remote_base + (local_fn - local_base)
}
