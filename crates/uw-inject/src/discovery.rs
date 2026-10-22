//! Find a running Proton game and what it takes to run another program in its Wine session: the
//! game's `WINEPREFIX` and `WINE*SYNC` settings (from `/proc/<pid>/environ`), its wineserver
//! socket, and Proton's `files` directory (from the prefix's `config_info`). Ported and renamed
//! from the injection spike; no dependency on `spikes/`.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::exe_matches;

/// The Proton `files` directory (holding `bin/wine`) from a prefix's `config_info`, whose lines
/// name paths under it such as `.../Proton 11.0/files/lib/`.
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

/// The game's Wine settings the helper must share to join its session: the prefix and the sync
/// mode (fsync/esync/ntsync must match the running wineserver).
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

/// Where a wineserver for the prefix with this device/inode listens (`server-<dev>-<ino>`).
pub fn server_socket(uid: u32, dev: u64, ino: u64) -> PathBuf {
    PathBuf::from(format!("/tmp/.wine-{uid}/server-{dev:x}-{ino:x}/socket"))
}

/// True when a `/proc/<pid>/cmdline` (NUL-separated argv) has an argv[0] naming `exe`.
/// Only argv[0]: Proton's `steam.exe` wrapper carries the Linux game path as argv[1].
pub fn cmdline_names(cmdline: &[u8], exe: &str) -> bool {
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
    exe_matches(&String::from_utf8_lossy(argv0), exe)
}

/// Pids whose argv0 names `exe`. Under Proton, a game's argv0 is its Windows path.
pub fn find_pids(exe: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut pids: Vec<i32> = dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid: &i32| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| cmdline_names(&cmdline, exe))
        })
        .collect();
    pids.sort_unstable();
    pids
}

/// The socket of the wineserver already running for `prefix`; an error if none is (Wine would
/// otherwise start a new, empty session the helper could not see the game in).
pub fn check_wine_session(prefix: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(prefix).map_err(|e| format!("prefix {}: {e}", prefix.display()))?;
    // SAFETY: getuid has no failure mode and no arguments.
    let uid = unsafe { libc::getuid() };
    let socket = server_socket(uid, meta.dev(), meta.ino());
    if socket.exists() {
        Ok(socket)
    } else {
        Err(format!("no running Wine session for {} (expected {})", prefix.display(), socket.display()))
    }
}

/// A typed discovery failure, so `uw-inject` can emit the design's `game_not_found` /
/// `no_wine_session` error codes instead of free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameError {
    /// The game exe is not running.
    NotRunning(String),
    /// The game runs but its Wine session could not be joined (no prefix, no wineserver, no
    /// Proton path, missing `wine`).
    NoWineSession(String),
}

impl GameError {
    /// The JSON-lines error code (design failure table, Linux side).
    pub fn code(&self) -> &'static str {
        match self {
            GameError::NotRunning(_) => "game_not_found",
            GameError::NoWineSession(_) => "no_wine_session",
        }
    }
}

impl std::fmt::Display for GameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameError::NotRunning(m) | GameError::NoWineSession(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for GameError {}

/// A running Proton game and its Wine session.
#[derive(Debug, Clone)]
pub struct GameSession {
    pub pid: i32,
    pub env: Vec<(String, String)>,
    pub prefix: PathBuf,
    /// Proton's `files` directory (holding `bin/wine`).
    pub proton: PathBuf,
}

impl GameSession {
    pub fn find(exe: &str, pid: Option<i32>) -> Result<GameSession, GameError> {
        GameSession::find_with_proton(exe, pid, None)
    }

    /// Like `find`, with an explicit Proton `files` directory instead of the one in `config_info`.
    pub fn find_with_proton(exe: &str, pid: Option<i32>, proton: Option<PathBuf>) -> Result<GameSession, GameError> {
        let pid = match pid {
            Some(p) => p,
            None => *find_pids(exe).first().ok_or_else(|| GameError::NotRunning(format!("{exe} is not running")))?,
        };
        let environ = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GameError::NotRunning(format!("process {pid} is not running"))
            } else {
                GameError::NoWineSession(format!("environ of {pid}: {e}"))
            }
        })?;
        let env = wine_env(&environ);
        let prefix = env
            .iter()
            .find(|(k, _)| k == "WINEPREFIX")
            .map(|(_, v)| PathBuf::from(v))
            .ok_or_else(|| GameError::NoWineSession(format!("pid {pid} has no WINEPREFIX (not a Wine process?)")))?;
        check_wine_session(&prefix).map_err(GameError::NoWineSession)?;
        let proton = match proton {
            Some(p) => p,
            None => {
                let info_path = config_info_for_prefix(&prefix);
                let info = std::fs::read_to_string(&info_path)
                    .map_err(|e| GameError::NoWineSession(format!("{}: {e}", info_path.display())))?;
                proton_files_from_config_info(&info)
                    .ok_or_else(|| GameError::NoWineSession(format!("no Proton path in {}", info_path.display())))?
            }
        };
        let wine = proton.join("bin/wine");
        if !wine.exists() {
            return Err(GameError::NoWineSession(format!("missing {}", wine.display())));
        }
        Ok(GameSession { pid, env, prefix, proton })
    }

    /// `wine <program> <args…>` in this game's Wine session, with our own shell's Wine variables
    /// removed so they cannot point the helper at another session.
    pub fn wine(&self, program: &Path, args: &[OsString]) -> Command {
        let mut cmd = Command::new(self.proton.join("bin/wine"));
        cmd.arg(program).args(args);
        for var in [
            "WINESERVER",
            "WINELOADER",
            "WINEDLLPATH",
            "WINEPREFIX",
            "WINEFSYNC",
            "WINEESYNC",
            "WINENTSYNC",
        ] {
            cmd.env_remove(var);
        }
        for (key, value) in &self.env {
            cmd.env(OsStr::new(key), value);
        }
        cmd
    }
}
