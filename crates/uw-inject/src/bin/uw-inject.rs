//! Linux launcher: find a running Proton game and drive the Wine-side helper.
//!
//!   uw-inject [--pid N] [--exe deadlock.exe] [--proton DIR] [--helper PATH]
//!             [--bootstrap PATH] [--overlay PATH] [--fallback]
//!
//! Finds the game and its Wine session, starts `uw-helper64.exe` inside it, prints the helper's
//! JSON-lines events, and — when an overlay is given — sends `load_overlay` once the bootstrap
//! connects fresh (the `Driver`). Defaults: `deadlock.exe`; Proton from the prefix's `config_info`;
//! helper and bootstrap from `~/.local/share/underwolf/`. Ctrl-C (SIGINT) or SIGTERM ends the
//! helper too; the pinned bootstrap stays in the game for the next run.

#[cfg(not(unix))]
fn main() {
    eprintln!("uw-inject is the Linux launcher");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("[uw-inject] {msg}");
            std::process::exit(1);
        }
    }
}

/// The running helper's pid, for the signal handler (0 = none yet).
#[cfg(unix)]
static HELPER_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Ending the client ends the helper (design "Reconnect after a client restart"): on SIGINT or
/// SIGTERM, kill the helper, then exit. The pinned bootstrap stays in the game. Only
/// async-signal-safe calls (an atomic load, `kill`, `_exit`) happen here.
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    let pid = HELPER_PID.load(std::sync::atomic::Ordering::SeqCst);
    // SAFETY: `kill` and `_exit` are async-signal-safe.
    unsafe {
        if pid > 0 {
            libc::kill(pid, libc::SIGKILL);
        }
        libc::_exit(130);
    }
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::Stdio;

    use uw_host::discovery::{helper_command, GameSession};
    use uw_host::driver::{Desired, Driver};
    use uw_host::{to_wine_path, RequestIds};
    use uw_pipe_proto::jsonl::Event;

    let mut pid: Option<i32> = None;
    let mut exe = "deadlock.exe".to_string();
    let mut proton: Option<PathBuf> = None;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let dir = home.join(".local/share/underwolf");
    let mut helper = dir.join("uw-helper64.exe");
    let mut bootstrap = dir.join("uw_bootstrap64.dll");
    let mut overlay: Option<PathBuf> = None;
    let mut fallback = false;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--pid" => pid = Some(value()?.parse().map_err(|e| format!("--pid: {e}"))?),
            "--exe" => exe = value()?,
            "--proton" => proton = Some(value()?.into()),
            "--helper" => helper = value()?.into(),
            "--bootstrap" => bootstrap = value()?.into(),
            "--overlay" => overlay = Some(value()?.into()),
            "--fallback" => fallback = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let session = match GameSession::find_with_proton(&exe, pid, proton) {
        Ok(s) => s,
        Err(e) => {
            // Report the design's Linux-side code on stdout; the detail goes to stderr.
            println!("{}", Event::error(e.code()).to_line());
            eprintln!("[uw-inject] {e}");
            return Ok(1);
        }
    };
    for p in [&helper, &bootstrap] {
        if !p.exists() {
            return Err(format!("missing {}", p.display()));
        }
    }
    if let Some(o) = &overlay
        && !o.exists()
    {
        return Err(format!("missing {}", o.display()));
    }
    eprintln!("[uw-inject] {exe} pid {}; prefix {}", session.pid, session.prefix.display());

    let mut cmd = helper_command(&session, &helper, &exe, &bootstrap, fallback);
    if std::env::var_os("WINEDEBUG").is_none() {
        cmd.env("WINEDEBUG", "-all");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running the helper: {e}"))?;
    HELPER_PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs a handler that only makes async-signal-safe calls.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut driver = Driver::new(Desired::new(overlay.as_deref().map(to_wine_path)), RequestIds::default());

    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("reading the helper: {e}"))?;
        let Ok(event) = Event::parse(&line) else {
            continue;
        };
        println!("{line}");
        if matches!(&event, Event::Error { code, .. } if code == "version_mismatch") {
            eprintln!("[uw-inject] restart the game to load the new bootstrap");
        }
        for cmd in driver.on_event(&event) {
            writeln!(stdin, "{}", cmd.to_line()).map_err(|e| format!("writing to the helper: {e}"))?;
            stdin.flush().ok();
        }
    }
    let status = child.wait().map_err(|e| format!("waiting for the helper: {e}"))?;
    Ok(status.code().unwrap_or(1))
}
