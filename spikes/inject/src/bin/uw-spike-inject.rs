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
    use std::ffi::OsString;
    use std::path::PathBuf;

    use uw_spike_inject::launcher::{GameSession, check_wine_session};
    use uw_spike_inject::to_wine_path;

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

    let session = GameSession::find_with_proton(&exe, pid, proton)?;
    for p in [injector.clone(), dll.clone()] {
        if !p.exists() {
            return Err(format!("missing {}", p.display()));
        }
    }

    eprintln!(
        "[uw-spike-inject] {exe} pid {}; prefix {}; wineserver {}; wine {}",
        session.pid,
        session.prefix.display(),
        check_wine_session(&session.prefix)?.display(),
        session.proton.join("bin/wine").display()
    );
    let mut cmd = session.wine(&injector, &[OsString::from(&exe), OsString::from(to_wine_path(&dll))]);
    if std::env::var_os("WINEDEBUG").is_none() {
        cmd.env("WINEDEBUG", "-all");
    }
    let status = cmd.status().map_err(|e| format!("running wine: {e}"))?;
    Ok(status.code().unwrap_or(1))
}
