//! Linux launcher: inject the C3 DLL into a running Proton game, with no setup on the game side.
//!
//!   uw-spike-inject [--pid N] [--exe deadlock.exe] [--proton DIR] [--injector PATH] [--dll PATH]
//!
//! Finds the game, copies its Wine settings, and runs `uw-spike-inject-win` with the game's own
//! Proton `wine` so it joins the game's running Wine session. Refuses to run when that session's
//! wineserver can't be found (Wine would start an empty session that can't see the game).
//! Defaults: `deadlock.exe`; Proton from the prefix's `config_info`; injector and DLL from
//! `~/.local/share/underwolf-spike/` (see `install.sh`).

#[cfg(unix)]
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("[uw-spike-inject] {msg}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("uw-spike-inject is the Linux launcher; on Windows run uw-spike-inject-win directly");
    std::process::exit(2);
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    use std::path::PathBuf;

    use uw_spike_inject::launcher::{check_wine_session, find_pids, wine_command};
    use uw_spike_inject::{config_info_for_prefix, proton_files_from_config_info, wine_env};

    let mut pid: Option<i32> = None;
    let mut exe = "deadlock.exe".to_string();
    let mut proton: Option<PathBuf> = None;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let install_dir = home.join(".local/share/underwolf-spike");
    let mut injector = install_dir.join("uw-spike-inject-win.exe");
    let mut dll = install_dir.join("uw_spike_input.dll");

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--pid" => pid = Some(value()?.parse().map_err(|e| format!("--pid: {e}"))?),
            "--exe" => exe = value()?,
            "--proton" => proton = Some(value()?.into()),
            "--injector" => injector = value()?.into(),
            "--dll" => dll = value()?.into(),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }

    let pid = match pid {
        Some(p) => p,
        None => *find_pids(&exe).first().ok_or(format!("{exe} is not running"))?,
    };
    let environ = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| format!("environ of {pid}: {e}"))?;
    let env = wine_env(&environ);
    let prefix = env
        .iter()
        .find(|(k, _)| k == "WINEPREFIX")
        .map(|(_, v)| PathBuf::from(v))
        .ok_or(format!("pid {pid} has no WINEPREFIX (not a Wine process?)"))?;
    let socket = check_wine_session(&prefix)?;

    let proton = match proton {
        Some(p) => p,
        None => {
            let info_path = config_info_for_prefix(&prefix);
            let info = std::fs::read_to_string(&info_path).map_err(|e| format!("{}: {e}", info_path.display()))?;
            proton_files_from_config_info(&info).ok_or(format!("no Proton path in {}", info_path.display()))?
        }
    };
    for p in [proton.join("bin/wine"), injector.clone(), dll.clone()] {
        if !p.exists() {
            return Err(format!("missing {}", p.display()));
        }
    }

    eprintln!(
        "[uw-spike-inject] {exe} pid {pid}; prefix {}; wineserver {}; wine {}",
        prefix.display(),
        socket.display(),
        proton.join("bin/wine").display()
    );
    let mut cmd = wine_command(&proton, &env, &injector, &exe, &dll);
    if std::env::var_os("WINEDEBUG").is_none() {
        cmd.env("WINEDEBUG", "-all");
    }
    let status = cmd.status().map_err(|e| format!("running wine: {e}"))?;
    Ok(status.code().unwrap_or(1))
}
