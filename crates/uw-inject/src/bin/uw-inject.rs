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
struct UserIds(std::sync::Mutex<UserIdState>);

#[cfg(unix)]
#[derive(Default)]
struct UserIdState {
    replies: std::collections::HashMap<u32, u32>,
    captures: std::collections::HashMap<u32, u32>,
    input: bool,
    routing: bool,
    configurations: std::collections::HashMap<u32, RoutingApplication>,
    requests: std::collections::HashMap<u32, RoutingApplication>,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct RoutingApplication {
    user: u32,
    epoch: u64,
    revision: Option<u64>,
}
#[cfg(unix)]
impl UserIdState {
    fn routing_result(&mut self, configuration: bool, result: &mut uw_pipe_proto::routing::RoutingResult) {
        let pending = if configuration { &mut self.configurations } else { &mut self.requests };
        if let Some(app) = pending.get(&result.request_id).copied()
            && app.epoch == result.epoch
            && app.revision.is_none_or(|revision| revision == result.revision)
        {
            pending.remove(&result.request_id);
            result.request_id = app.user;
        }
    }
}
#[cfg(unix)]
impl UserIds {
    fn outgoing(&self, line: &str, host_id: u32) -> Result<uw_pipe_proto::jsonl::Command, String> {
        use uw_pipe_proto::jsonl::{Command, RoutingCommand};
        let (cmd, user) = Command::parse_with_id(line, host_id).map_err(|e| e.to_string())?;
        let mut state = self.0.lock().unwrap();
        let application = match &cmd {
            Command::Routing(RoutingCommand::ConfigureInput { config, .. }) => {
                Some((true, config.epoch, Some(config.revision)))
            }
            Command::Routing(RoutingCommand::RouteRequest { request, .. }) => {
                Some((false, request.epoch, Some(request.revision)))
            }
            Command::SetState { input_epoch: Some(epoch), edit_mode: Some(_), .. } if state.routing => {
                Some((false, *epoch, None))
            }
            _ => None,
        };
        if let Some((configuration, epoch, revision)) = application {
            let pending = if configuration { &mut state.configurations } else { &mut state.requests };
            if pending.len() >= 32 || pending.contains_key(&host_id) {
                return Err("routing_requests_full".into());
            }
            pending.insert(host_id, RoutingApplication { user: user.unwrap_or(host_id), epoch, revision });
        }
        if state.input && !state.routing && matches!(cmd, Command::SetState { edit_mode: Some(_), .. }) {
            if state.captures.len() >= 32 {
                return Err("input_requests_full".into());
            }
            state.captures.insert(host_id, user.unwrap_or(host_id));
        }
        if let Some(user) = user
            && !matches!(
                cmd,
                Command::Routing(RoutingCommand::ConfigureInput { .. } | RoutingCommand::RouteRequest { .. })
            )
        {
            state.replies.insert(host_id, user);
        }
        Ok(cmd)
    }

    fn incoming(&self, event: &mut uw_pipe_proto::jsonl::Event) {
        use uw_pipe_proto::jsonl::{Event, RoutingEvent};
        let mut state = self.0.lock().unwrap();
        match event {
            Event::InputReady { .. } => {
                state.input = true;
                state.routing = false;
            }
            Event::Routing(RoutingEvent::RoutingReady { .. }) => {
                state.input = true;
                state.routing = true;
            }
            Event::Routing(RoutingEvent::RoutingConfigured { result }) => {
                state.routing_result(true, result);
                return;
            }
            Event::Routing(RoutingEvent::RoutingResult { result }) => {
                state.routing_result(false, result);
                return;
            }
            Event::OverlayDisconnected | Event::GameExited | Event::Stopped { .. } => {
                state.captures.clear();
                state.configurations.clear();
                state.requests.clear();
                state.input = false;
                state.routing = false;
            }
            Event::CaptureResult { capture_request_id, .. } => {
                if let Some(user) = state.captures.remove(capture_request_id) {
                    *capture_request_id = user;
                }
                return;
            }
            Event::Error { request_id: Some(id), .. } => {
                let routing_user =
                    state.configurations.remove(id).or_else(|| state.requests.remove(id)).map(|app| app.user);
                if let Some(user) = routing_user.or_else(|| state.captures.remove(id)) {
                    state.replies.remove(id);
                    *id = user;
                    return;
                }
            }
            _ => {}
        }
        if let Some(id) = event.request_id_mut()
            && let Some(user) = state.replies.remove(id)
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

/// Hand each non-blank line of `input` to `each`, until EOF or a read error.
#[cfg(unix)]
fn command_lines(mut input: impl std::io::BufRead, mut each: impl FnMut(&str)) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Bytes, not `lines()`: a line that is not UTF-8 must not end the reader.
        match input.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if !line.trim().is_empty() {
            each(line);
        }
    }
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
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
        command_lines(std::io::stdin().lock(), |line| match ids.outgoing(line, commands.next_request_id()) {
            Ok(cmd) => commands.send(cmd),
            Err(e) => eprintln!("[uw-inject] bad command: {e}"),
        });
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

    #[test]
    fn capture_id_survives_acceptance_until_application_or_loss() {
        use uw_pipe_proto::input::CaptureCode;
        let ids = UserIds::default();
        ids.incoming(&mut Event::InputReady { version: 1 });
        ids.outgoing(r#"{"cmd":"set_state","request_id":42,"input_epoch":1,"edit_mode":true}"#, 7).unwrap();
        let mut accepted = Event::StateSet { request_id: 7, seq: 2 };
        ids.incoming(&mut accepted);
        assert_eq!(accepted.request_id(), Some(42));
        let mut applied =
            Event::CaptureResult { epoch: 1, state_seq: 2, capture_request_id: 7, code: CaptureCode::Applied };
        ids.incoming(&mut applied);
        assert!(matches!(applied, Event::CaptureResult { capture_request_id: 42, .. }));
        ids.outgoing(r#"{"cmd":"set_state","request_id":43,"input_epoch":1,"edit_mode":true}"#, 8).unwrap();
        ids.incoming(&mut Event::OverlayDisconnected);
        let mut late =
            Event::CaptureResult { epoch: 1, state_seq: 2, capture_request_id: 8, code: CaptureCode::Applied };
        ids.incoming(&mut late);
        assert!(matches!(late, Event::CaptureResult { capture_request_id: 8, .. }));
    }

    #[test]
    fn routing_application_ids_require_exact_purpose_epoch_and_revision() {
        use uw_pipe_proto::{
            jsonl::{RoutingCommand, RoutingEvent},
            routing::*,
        };
        let ids = UserIds::default();
        let config = RoutingConfig {
            epoch: 1,
            revision: 3,
            request_id: 42,
            width: 100,
            height: 100,
            widgets: vec![],
            hotkeys: vec![HotkeySpec {
                id: 1,
                vk: 9,
                modifiers: 8,
                flags: 0,
                action: HotkeyAction::ToggleEdit,
                target: None,
            }],
        };
        ids.outgoing(&RoutingCommand::ConfigureInput { request_id: 42, config }.to_line().unwrap(), 7).unwrap();
        let result = RoutingResult { epoch: 1, revision: 3, request_id: 7, code: RoutingCode::Applied, through_seq: 0 };
        let mut wrong_purpose = Event::Routing(RoutingEvent::RoutingResult { result });
        ids.incoming(&mut wrong_purpose);
        assert_eq!(wrong_purpose.request_id(), Some(7));
        let mut wrong_revision =
            Event::Routing(RoutingEvent::RoutingConfigured { result: RoutingResult { revision: 4, ..result } });
        ids.incoming(&mut wrong_revision);
        assert_eq!(wrong_revision.request_id(), Some(7));
        let mut wrong_epoch =
            Event::Routing(RoutingEvent::RoutingConfigured { result: RoutingResult { epoch: 2, ..result } });
        ids.incoming(&mut wrong_epoch);
        assert_eq!(wrong_epoch.request_id(), Some(7));
        let mut exact = Event::Routing(RoutingEvent::RoutingConfigured { result });
        ids.incoming(&mut exact);
        assert_eq!(exact.request_id(), Some(42));
    }
    #[test]
    fn v2_set_state_mapping_survives_state_acceptance_until_application() {
        use uw_pipe_proto::{jsonl::RoutingEvent, routing::*};
        let ids = UserIds::default();
        ids.incoming(&mut Event::Routing(RoutingEvent::RoutingReady { version: 2 }));
        ids.outgoing(r#"{"cmd":"set_state","request_id":42,"input_epoch":1,"edit_mode":true}"#, 7).unwrap();
        let mut accepted = Event::StateSet { request_id: 7, seq: 2 };
        ids.incoming(&mut accepted);
        assert_eq!(accepted.request_id(), Some(42));
        let mut applied = Event::Routing(RoutingEvent::RoutingResult {
            result: RoutingResult { epoch: 1, revision: 3, request_id: 7, code: RoutingCode::Applied, through_seq: 0 },
        });
        ids.incoming(&mut applied);
        assert_eq!(applied.request_id(), Some(42));
    }
    #[test]
    fn routing_application_maps_are_bounded_and_loss_clears_both_kinds() {
        use uw_pipe_proto::{
            jsonl::{RoutingCommand, RoutingEvent},
            routing::*,
        };
        let ids = UserIds::default();
        let config = RoutingConfig {
            epoch: 1,
            revision: 3,
            request_id: 42,
            width: 100,
            height: 100,
            widgets: vec![],
            hotkeys: vec![HotkeySpec {
                id: 1,
                vk: 9,
                modifiers: 8,
                flags: 0,
                action: HotkeyAction::ToggleEdit,
                target: None,
            }],
        };
        let line = RoutingCommand::ConfigureInput { request_id: 42, config }.to_line().unwrap();
        for host_id in 1..=32 {
            ids.outgoing(&line, host_id).unwrap();
        }
        assert!(ids.outgoing(&line, 33).is_err());
        let request = RoutingRequest {
            epoch: 1,
            revision: 3,
            request_id: 43,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        };
        ids.outgoing(&RoutingCommand::RouteRequest { request_id: 43, request }.to_line().unwrap(), 100).unwrap();
        let mut lost = Event::reply_error(1, "routing_application_timeout");
        ids.incoming(&mut lost);
        assert_eq!(lost.request_id(), Some(42));
        ids.outgoing(&line, 33).unwrap();
        ids.incoming(&mut Event::OverlayDisconnected);
        let mut late = Event::Routing(RoutingEvent::RoutingResult {
            result: RoutingResult {
                epoch: 1,
                revision: 3,
                request_id: 100,
                code: RoutingCode::Applied,
                through_seq: 0,
            },
        });
        ids.incoming(&mut late);
        assert_eq!(late.request_id(), Some(100));
        let state = ids.0.lock().unwrap();
        assert!(state.configurations.is_empty() && state.requests.is_empty());
    }
    /// Catches a line that is not UTF-8 ending the reader: every later command would be ignored.
    #[test]
    fn a_line_that_is_not_utf8_is_passed_on_and_later_commands_still_arrive() {
        let mut seen = Vec::new();
        super::command_lines(&b"\xff\n\n{\"cmd\":\"ping\"}\r\n"[..], |l| seen.push(l.to_string()));
        assert_eq!(seen, ["\u{fffd}", r#"{"cmd":"ping"}"#]);
    }
}
