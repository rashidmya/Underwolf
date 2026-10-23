//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Creates the control channel (doorbells, mask region,
//! state block, overlay pipe) and the bootstrap pipe, finds the game's UI thread and sets a thread
//! hook that makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already
//! pinned, skips the hook), then serves both pipes and relays JSON lines: commands
//! (`load_overlay`, `set_state`, `ping`) on stdin, events on stdout, each reply echoing its
//! command's `request_id`; diagnostics on stderr.
//!
//! Modules: `win32` (the real `Win32` for `decide`, and the channel), `pipe_server` (the
//! overlapped pipe server both pipes use), `commands` (the stdin reader; stdin closing ends the
//! helper in every phase), `relay` (the one wait loop, and the game watcher).

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-helper64 runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod pipe_server;
#[cfg(windows)]
mod relay;
#[cfg(windows)]
mod win32;

#[cfg(windows)]
fn main() {
    std::process::exit(helper_main());
}

#[cfg(windows)]
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A wait timeout in ms for the Win32 wait functions, never `INFINITE`.
#[cfg(windows)]
pub(crate) fn wait_ms(ms: u64) -> u32 {
    use windows_sys::Win32::System::Threading::INFINITE;
    u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1)
}

#[cfg(windows)]
fn helper_main() -> i32 {
    use uw_helper::decide::{run, Config, Outcome};

    let mut args = std::env::args().skip(1);
    let exe = args.next().unwrap_or_default();
    let mut bootstrap = String::new();
    let mut force_fallback = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bootstrap" => bootstrap = args.next().unwrap_or_default(),
            "--fallback" => force_fallback = true,
            other => {
                eprintln!("[uw-helper64] unknown argument {other}");
                return 2;
            }
        }
    }
    if exe.is_empty() || bootstrap.is_empty() {
        eprintln!("usage: uw-helper64 <game-exe> --bootstrap <path> [--fallback]");
        return 2;
    }

    // Read stdin from the start, so its closing ends the helper in every phase.
    let commands = match commands::spawn_stdin_reader() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[uw-helper64] {e}");
            return 1;
        }
    };
    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
    // do not block for two minutes; unset in production, the defaults stand.
    let env_ms = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let defaults = Config::default();
    let cfg = Config {
        force_fallback,
        window_timeout_ms: env_ms("UW_WINDOW_TIMEOUT_MS", defaults.window_timeout_ms),
        hello_timeout_ms: env_ms("UW_HELLO_TIMEOUT_MS", defaults.hello_timeout_ms),
        window_poll_ms: defaults.window_poll_ms,
    };
    // The slow-overlay tests shorten the `status` and `ping` timeouts the same way.
    let status_timeout_ms = env_ms("UW_STATUS_TIMEOUT_MS", pipe_server::RELAY_TIMEOUT_MS);
    let ping_timeout_ms = env_ms("UW_PING_TIMEOUT_MS", pipe_server::RELAY_TIMEOUT_MS);
    let outcome = run(&mut w, &cfg);
    commands.refuse_queued();
    relay::emit(&outcome.to_event());
    match outcome {
        Outcome::Connected { .. } => {
            let pid = w.pid;
            if let (Some(channel), Some(pipe)) = (w.channel.as_mut(), w.pipe.as_mut()) {
                relay::run(pid, &commands, channel, pipe, status_timeout_ms, ping_timeout_ms);
            }
            0
        }
        // The game ended before the bootstrap connected: the same report, and the same exit
        // code, as the post-connect watcher's `game_exited`.
        Outcome::GameExited => 0,
        other => {
            eprintln!("[uw-helper64] {other:?}");
            1
        }
    }
}
