#![cfg(unix)]
//! The real runtime against fake helpers: small `sh` scripts that print events and read commands,
//! started through a scripted `Launch`. They check the process wiring (stdin, stdout, exit, a real
//! restart, stop on close) that the supervisor's unit tests cannot.

use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use uw_host::driver::Desired;
use uw_host::host::{Host, Launch};
use uw_pipe_proto::jsonl::{Command as HelperCommand, Event};

/// Starts `sh -c <script> uw-helper64 <args>…` for each start, in order (the script sees the
/// host's extra arguments as `$@`); past the last one, `game_not_found`.
struct Scripts(Vec<&'static str>);

impl Launch for Scripts {
    fn command(&mut self, args: &[String]) -> Result<Command, String> {
        if self.0.is_empty() {
            return Err("game_not_found".into());
        }
        let mut c = Command::new("sh");
        c.arg("-c").arg(self.0.remove(0)).arg("uw-helper64").args(args);
        Ok(c)
    }
}

fn next(host: &Host) -> Event {
    host.recv_timeout(Duration::from_secs(10)).expect("an event within 10 s")
}

/// Catches commands not reaching the helper's stdin, its stdout not being read, a dead helper not
/// restarted, or a stop that does not close stdin (the second script waits on it).
#[test]
fn a_fake_helper_is_driven_restarted_and_stopped() {
    let host = Host::start(
        Scripts(vec![
            // Connect, echo the first command back (not an event: a protocol error), then die.
            r#"echo '{"event":"bootstrap_connected","overlay_loaded":false}'; read -r line; printf 'got %s\n' "$line"; exit 1"#,
            // Report the game gone, then wait for stdin to close.
            r#"echo '{"event":"game_exited"}'; read -r line; exit 0"#,
        ]),
        Desired::new(Some(r"Z:\o.dll".into())),
    );
    assert_eq!(next(&host), Event::BootstrapConnected { overlay_loaded: false, overlay_path: None });
    assert_eq!(
        next(&host),
        Event::HelperProtocolError { line: r#"got {"cmd":"load_overlay","request_id":1,"path":"Z:\\o.dll"}"#.into() }
    );
    assert_eq!(next(&host), Event::reply_error(1, "bootstrap_not_connected"));
    assert_eq!(next(&host), Event::HelperRestarted { attempt: 1 });
    assert_eq!(next(&host), Event::GameExited);
    assert_eq!(next(&host), Event::Stopped { reason: "game_exited".into() });
    // The host closed the script's stdin, it exited, and the host ended.
    assert_eq!(host.recv_timeout(Duration::from_secs(10)), Err(RecvTimeoutError::Disconnected));
}

/// Catches a user command losing its id on the way, or a reply not reaching the user.
#[test]
fn a_user_command_and_its_reply_keep_their_request_id() {
    let host = Host::start(
        Scripts(vec![
            r#"read -r line; id=$(echo "$line" | sed 's/.*"request_id":\([0-9]*\).*/\1/'); echo "{\"event\":\"pong\",\"request_id\":$id,\"seen_seq\":4}"; read rest"#,
        ]),
        Desired::new(None),
    );
    let id = host.next_request_id();
    host.send(HelperCommand::Ping { request_id: id });
    assert_eq!(next(&host), Event::Pong { request_id: id, seen_seq: 4 });
    // Dropping the host closes the script's stdin; it must not be left running.
}

/// Catches a failed start that loops instead of stopping.
#[test]
fn a_helper_that_cannot_start_stops_the_host() {
    let host = Host::start(Scripts(vec![]), Desired::new(None));
    assert_eq!(next(&host), Event::error("game_not_found"));
    assert_eq!(next(&host), Event::Stopped { reason: "game_not_found".into() });
    assert_eq!(host.recv_timeout(Duration::from_secs(5)), Err(RecvTimeoutError::Disconnected));
}

/// Catches the stdout reader ending at a line that is not UTF-8: that line must become a
/// `helper_protocol_error` and the events after it must still arrive.
#[test]
fn a_non_utf8_line_is_a_protocol_error_and_later_events_still_arrive() {
    let host = Host::start(
        Scripts(vec![r#"printf '\377\n'; echo '{"event":"game_exited"}'; read -r line; exit 0"#]),
        Desired::new(None),
    );
    assert_eq!(next(&host), Event::HelperProtocolError { line: "\u{fffd}".into() });
    assert_eq!(next(&host), Event::GameExited);
    assert_eq!(next(&host), Event::Stopped { reason: "game_exited".into() });
}

/// Catches the resume point not reaching a restarted helper's command line (design 2b
/// "uw-host": `--data-resume <producer_id>:<seq>`, the seq of the last `data_event` delivered),
/// or data events not passed to the user unchanged.
#[test]
fn a_restarted_helper_gets_data_resume_from_the_last_data_event() {
    let connected = r#"{"event":"data_connected","channel":1,"producer_id":77,"from_seq":3,"schema":[]}"#;
    let tick = r#"{"event":"data_event","channel":1,"seq":4,"name":"tick","data":"1","ts":9}"#;
    let host = Host::start(
        Scripts(vec![
            // Both lines, then die.
            r#"echo '{"event":"data_connected","channel":1,"producer_id":77,"from_seq":3,"schema":[]}'; echo '{"event":"data_event","channel":1,"seq":4,"name":"tick","data":"1","ts":9}'; exit 1"#,
            // Print the arguments it got (not an event: a protocol error), then wait for stdin.
            r#"printf 'args %s\n' "$*"; read -r line; exit 0"#,
        ]),
        Desired::new(None),
    );
    assert_eq!(next(&host), Event::parse(connected).unwrap());
    assert_eq!(next(&host), Event::parse(tick).unwrap());
    assert_eq!(next(&host), Event::HelperRestarted { attempt: 1 });
    assert_eq!(next(&host), Event::HelperProtocolError { line: "args --data-resume 77:4".into() });
}
