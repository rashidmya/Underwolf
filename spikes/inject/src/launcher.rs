//! Linux side: run the Windows injector inside a running Proton game's Wine session.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{config_info_for_prefix, exe_matches, proton_files_from_config_info, server_socket, to_wine_path, wine_env};

/// `wine <program> <args…>` with Proton's `wine` from `proton_files` and the game's Wine settings
/// (`env`). Our own shell's Wine variables are removed so they can't point at another session.
fn wine_in_session<I, S>(proton_files: &Path, env: &[(String, String)], program: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(proton_files.join("bin/wine"));
    cmd.arg(program).args(args);
    for var in ["WINESERVER", "WINELOADER", "WINEDLLPATH", "WINEPREFIX"] {
        cmd.env_remove(var);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd
}

/// `wine <injector> <target_exe> <dll>` with the game's Proton `wine` and the game's Wine settings.
pub fn wine_command(
    proton_files: &Path,
    env: &[(String, String)],
    injector: &Path,
    target_exe: &str,
    dll: &Path,
) -> Command {
    wine_in_session(proton_files, env, injector, [OsString::from(target_exe), OsString::from(to_wine_path(dll))])
}

/// A running Proton game and what it takes to run another program in its Wine session.
#[derive(Debug, Clone)]
pub struct GameSession {
    pub pid: i32,
    /// The game's Wine settings (`wine_env` of its environment).
    pub env: Vec<(String, String)>,
    pub prefix: PathBuf,
    /// Proton's `files` directory (holding `bin/wine`).
    pub proton: PathBuf,
}

impl GameSession {
    /// Finds `exe` (or takes `pid`), reads its Wine settings, checks that its wineserver is
    /// running and locates Proton from the prefix's `config_info`.
    pub fn find(exe: &str, pid: Option<i32>) -> Result<GameSession, String> {
        GameSession::find_with_proton(exe, pid, None)
    }

    /// Like `find`, with an explicit Proton `files` directory instead of the one in `config_info`.
    pub fn find_with_proton(exe: &str, pid: Option<i32>, proton: Option<PathBuf>) -> Result<GameSession, String> {
        let pid = match pid {
            Some(p) => p,
            None => *find_pids(exe).first().ok_or(format!("{exe} is not running"))?,
        };
        let environ = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| format!("environ of {pid}: {e}"))?;
        let env = wine_env(&environ);
        let prefix = env
            .iter()
            .find(|(k, _)| k == "WINEPREFIX")
            .map(|(_, v)| PathBuf::from(v))
            .ok_or(format!("pid {pid} has no WINEPREFIX (not a Wine process?)"))?;
        check_wine_session(&prefix)?;
        let proton = match proton {
            Some(p) => p,
            None => {
                let info_path = config_info_for_prefix(&prefix);
                let info =
                    std::fs::read_to_string(&info_path).map_err(|e| format!("{}: {e}", info_path.display()))?;
                proton_files_from_config_info(&info).ok_or(format!("no Proton path in {}", info_path.display()))?
            }
        };
        let wine = proton.join("bin/wine");
        if !wine.exists() {
            return Err(format!("missing {}", wine.display()));
        }
        Ok(GameSession { pid, env, prefix, proton })
    }

    /// `wine <program> <args…>` in this game's Wine session.
    pub fn wine(&self, program: &Path, args: &[OsString]) -> Command {
        wine_in_session(&self.proton, &self.env, program, args)
    }
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
