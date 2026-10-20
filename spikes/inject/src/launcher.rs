//! Linux side: run the Windows injector inside a running Proton game's Wine session.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{exe_matches, server_socket, to_wine_path};

/// `wine <injector> <target_exe> <dll>` with the game's Proton `wine` and the game's Wine settings.
pub fn wine_command(
    proton_files: &Path,
    env: &[(String, String)],
    injector: &Path,
    target_exe: &str,
    dll: &Path,
) -> Command {
    let mut cmd = Command::new(proton_files.join("bin/wine"));
    cmd.arg(injector).arg(target_exe).arg(to_wine_path(dll));
    for var in ["WINESERVER", "WINELOADER", "WINEDLLPATH", "WINEPREFIX"] {
        cmd.env_remove(var);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd
}

/// The socket of the wineserver already running for `prefix`. Wine would otherwise start a new,
/// empty session for the injector, which can't see the game.
pub fn check_wine_session(prefix: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(prefix).map_err(|e| format!("prefix {}: {e}", prefix.display()))?;
    let uid = unsafe { libc::getuid() };
    let socket = server_socket(uid, meta.dev(), meta.ino());
    if socket.exists() {
        Ok(socket)
    } else {
        Err(format!(
            "no running Wine session for {} (expected {})",
            prefix.display(),
            socket.display()
        ))
    }
}

/// Pids whose argv0 is `exe`. Under Proton, a game's argv0 is its Windows path.
pub fn find_pids(exe: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut pids: Vec<i32> = dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid: &i32| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
                let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
                exe_matches(&String::from_utf8_lossy(argv0), exe)
            })
        })
        .collect();
    pids.sort_unstable();
    pids
}
