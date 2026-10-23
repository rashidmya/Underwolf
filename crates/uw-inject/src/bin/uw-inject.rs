//! Linux launcher: a thin CLI on `uw-host` (design 2a).
//!
//!   uw-inject [--pid N] [--exe deadlock.exe] [--proton DIR] [--helper PATH]
//!             [--bootstrap PATH] [--overlay PATH] [--fallback]
//!
//! Finds the game and its Wine session, then hands over to `uw_host::host::Host`, which starts
//! `uw-helper64.exe` in that session, restarts it if it dies, and keeps the overlay at the desired
//! state (loaded, then every `set_state` it was given). JSON commands on stdin (`request_id`
//! optional: assigned if absent), events on stdout, diagnostics on stderr. The `request_id` you
//! send comes back on your reply; the host's own replies may use the same number. Exits 0 after
//! `stopped {"reason":"game_exited"}` and 1 after any other stop. Defaults: `deadlock.exe`;
//! Proton from the prefix's `config_info`; helper and bootstrap from `~/.local/share/underwolf/`.
//! Ctrl-C (SIGINT) or SIGTERM exits at once: the helper's stdin closes with us, so it exits too
//! (design "Failures"), and the pinned bootstrap stays in the game for the next run.

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

/// The request ids the user chose, by the id the host gave the command. Every command goes to the
/// host with an id from `HostHandle::next_request_id`, never with the user's own, so a user's id
/// cannot collide with one the host took for its reconcile commands (two pending commands would
/// then share it in the host and the helper); the reply gets the user's id back.
#[cfg(unix)]
#[derive(Default)]
struct UserIds(std::sync::Mutex<std::collections::HashMap<u32, u32>>);

#[cfg(unix)]
impl UserIds {
    /// Parse one stdin line into a command carrying `host_id`; remember the user's id, if any.
    fn outgoing(&self, line: &str, host_id: u32) -> Result<uw_pipe_proto::jsonl::Command, String> {
        let (cmd, user) = uw_pipe_proto::jsonl::Command::parse_with_id(line, host_id).map_err(|e| e.to_string())?;
        if let Some(user) = user {
            self.0.lock().unwrap().insert(host_id, user);
        }
        Ok(cmd)
    }

    /// Put the user's id back on a reply (once: the entry is forgotten).
    fn incoming(&self, event: &mut uw_pipe_proto::jsonl::Event) {
        if let Some(id) = event.request_id_mut()
            && let Some(user) = self.0.lock().unwrap().remove(id)
        {
            *id = user;
        }
    }
}

/// On SIGINT or SIGTERM, exit at once. Only `_exit` (async-signal-safe) happens here; the kernel
/// then closes the helper's stdin, and the helper exits on its own.
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    // SAFETY: `_exit` is async-signal-safe.
    unsafe { libc::_exit(130) };
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::sync::Arc;

    use uw_host::discovery::GameSession;
    use uw_host::driver::Desired;
    use uw_host::host::{Host, WineLaunch};
    use uw_host::to_wine_path;
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

    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs a handler that only makes an async-signal-safe call.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    let host = Host::start(
        WineLaunch { session, exe, helper, bootstrap, fallback },
        Desired::new(overlay.as_deref().map(to_wine_path)),
    );
    let commands = host.handle();
    let user_ids = Arc::new(UserIds::default());
    let ids = Arc::clone(&user_ids);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match ids.outgoing(&line, commands.next_request_id()) {
                Ok(cmd) => commands.send(cmd),
                Err(e) => eprintln!("[uw-inject] bad command: {e}"),
            }
        }
        // Our stdin closing does not stop us: Ctrl-C, a signal or a stop does.
    });

    let mut code = 1;
    while let Some(mut event) = host.recv() {
        user_ids.incoming(&mut event);
        println!("{}", event.to_line());
        match &event {
            Event::Error { code, .. } if code == "version_mismatch" => {
                eprintln!("[uw-inject] restart the game to load the new bootstrap or overlay");
            }
            Event::Stopped { reason } => code = if reason == "game_exited" { 0 } else { 1 },
            _ => {}
        }
    }
    Ok(code)
}

#[cfg(all(test, unix))]
mod tests {
    use super::UserIds;
    use uw_pipe_proto::jsonl::{Command, Event};

    /// Catches a user's id reaching the host and the helper: if it equals one the host assigned
    /// (here 1, the host's own reconcile `load_overlay`), two pending commands would share it. The
    /// reply must still carry the user's id.
    #[test]
    fn a_users_id_never_reaches_the_helper_and_comes_back_on_the_reply() {
        let ids = UserIds::default();
        let cmd = ids.outgoing(r#"{"cmd":"ping","request_id":1}"#, 2).unwrap();
        assert_eq!(cmd, Command::Ping { request_id: 2 });
        // The host's own reply keeps its id; the user's comes back as 1, once.
        let mut host_reply = Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() };
        ids.incoming(&mut host_reply);
        assert_eq!(host_reply.request_id(), Some(1));
        let mut pong = Event::Pong { request_id: 2, seen_seq: 0 };
        ids.incoming(&mut pong);
        assert_eq!(pong, Event::Pong { request_id: 1, seen_seq: 0 });
        let mut again = Event::Pong { request_id: 2, seen_seq: 0 };
        ids.incoming(&mut again);
        assert_eq!(again.request_id(), Some(2));
    }

    #[test]
    fn a_command_without_an_id_keeps_the_hosts() {
        let ids = UserIds::default();
        assert_eq!(ids.outgoing(r#"{"cmd":"ping"}"#, 5).unwrap(), Command::Ping { request_id: 5 });
        let mut error = Event::reply_error(5, "overlay_not_connected");
        ids.incoming(&mut error);
        assert_eq!(error, Event::reply_error(5, "overlay_not_connected"));
        assert!(ids.outgoing("not json", 6).is_err());
    }
}
