//! The supervisor against a scripted fake helper on a fake clock: the test plays the helper by
//! feeding its stdout lines and exits, and checks what the host would send and emit.

use uw_host::driver::Desired;
use uw_host::supervisor::{Action, Input, Supervisor, OVERLAY_TIMEOUT_MS};
use uw_host::RequestIds;
use uw_pipe_proto::data::{ResumePoint, SchemaCategory};
use uw_pipe_proto::jsonl::{Command, Event};

const OVERLAY: &str = r"Z:\games\overlay.dll";

struct Fake {
    sup: Supervisor,
    ids: RequestIds,
    now: u64,
}

impl Fake {
    /// A started host with `OVERLAY` desired.
    fn started() -> Fake {
        let ids = RequestIds::default();
        let mut sup = Supervisor::new(Desired::new(Some(OVERLAY.into())), ids.clone());
        assert_eq!(sup.start(0), vec![start_helper(&[])]);
        Fake { sup, ids, now: 0 }
    }
    /// The fake helper prints `event`.
    fn says(&mut self, event: Event) -> Vec<Action> {
        self.sup.handle(Input::HelperLine(event.to_line()), self.now)
    }
    fn exits(&mut self) -> Vec<Action> {
        self.sup.handle(Input::HelperExited, self.now)
    }
    fn user(&mut self, cmd: Command) -> Vec<Action> {
        self.sup.handle(Input::User(cmd), self.now)
    }
    fn at(&mut self, ms: u64) -> Vec<Action> {
        self.now = ms;
        self.sup.tick(ms)
    }
}

fn emit(e: Event) -> Action {
    Action::Emit(e)
}

fn start_helper(args: &[&str]) -> Action {
    Action::StartHelper { args: args.iter().map(|a| a.to_string()).collect() }
}

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
}

/// Catches reconcile commands reusing a user's id, a reply not clearing its command, or a dead
/// helper leaving a command unanswered (or answering one twice).
#[test]
fn request_ids_are_shared_and_replies_clear_their_commands() {
    let mut f = Fake::started();
    let ping = Command::Ping { request_id: f.ids.next() }; // 1
    assert_eq!(f.user(ping.clone()), vec![Action::Send(ping)]);
    // The reconcile load takes the next id from the same counter.
    assert_eq!(
        f.says(bootstrap_connected(false)),
        vec![emit(bootstrap_connected(false)), Action::Send(Command::LoadOverlay { request_id: 2, path: OVERLAY.into() })]
    );
    let pong = Event::Pong { request_id: 1, seen_seq: 0 };
    assert_eq!(f.says(pong.clone()), vec![emit(pong)]);
    // The helper dies: only the unanswered load (2) fails, once.
    let actions = f.exits();
    assert_eq!(actions, vec![emit(Event::reply_error(2, "bootstrap_not_connected"))]);
    // A command while no helper runs fails at once and is not sent.
    let set = Command::SetState { input_epoch: None, request_id: f.ids.next(), edit_mode: Some(true), visible: None };
    assert_eq!(f.user(set), vec![emit(Event::reply_error(3, "overlay_not_connected"))]);
}

/// Catches a wrong back-off (1, 2, 4, 8 s, then every 10 s), a delay that never resets after a
/// helper stayed up 60 s, or one that resets earlier (or counts from the last exit, not the start).
#[test]
fn restarts_back_off_and_reset_after_a_minute_of_uptime() {
    let mut f = Fake::started();
    let mut t = 0;
    for (attempt, delay) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (5, 10_000), (6, 10_000)] {
        t += 100;
        f.now = t;
        assert_eq!(f.exits(), vec![]);
        assert_eq!(f.sup.next_deadline(), Some(t + delay), "attempt {attempt}");
        assert_eq!(f.at(t + delay - 1), vec![], "too early for attempt {attempt}");
        t += delay;
        assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt }), start_helper(&[])]);
    }
    // This helper stays up 1 ms short of a minute: no reset, the delay stays at 10 s.
    t += 59_999;
    f.now = t;
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 10_000));
    t += 10_000;
    assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt: 7 }), start_helper(&[])]);
    // This one stays up a minute: the next delay is the first one again.
    t += 60_000;
    f.now = t;
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 1_000));
    assert_eq!(f.at(t + 1_000), vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&[])]);
}

/// Catches a reconnect that reloads a loaded overlay, skips the full state push, or loses a
/// change made while the helper was down.
#[test]
fn reconcile_after_reconnect_loads_only_when_needed_and_pushes_the_desired_state() {
    let mut f = Fake::started();
    let on = Command::SetState { input_epoch: None, request_id: f.ids.next(), edit_mode: Some(true), visible: None }; // 1
    assert_eq!(f.user(on.clone()), vec![Action::Send(on)]);
    let connected = Event::OverlayConnected { module_version: 1 };
    assert_eq!(
        f.says(connected.clone()),
        vec![
            emit(connected.clone()),
            Action::Send(Command::SetState { input_epoch: None, request_id: 2, edit_mode: Some(true), visible: Some(true) })
        ]
    );

    // The helper dies. While it is down the user turns edit mode off: refused, but remembered.
    f.now = 5_000;
    f.exits();
    let off = Command::SetState { input_epoch: None, request_id: f.ids.next(), edit_mode: Some(false), visible: None }; // 3
    assert_eq!(f.user(off), vec![emit(Event::reply_error(3, "overlay_not_connected"))]);
    assert!(!f.sup.desired().edit_mode);

    // Restarted: the pinned bootstrap still runs the overlay, so no load; the overlay
    // reconnects and gets the full desired state, with edit mode off.
    f.at(6_000);
    assert_eq!(f.says(bootstrap_connected(true)), vec![emit(bootstrap_connected(true))]);
    assert_eq!(
        f.says(connected.clone()),
        vec![emit(connected), Action::Send(Command::SetState { input_epoch: None, request_id: 4, edit_mode: Some(false), visible: Some(true) })]
    );

    // A bootstrap that lost its overlay (a fresh game) is told to load it again.
    assert_eq!(
        f.says(bootstrap_connected(false)),
        vec![emit(bootstrap_connected(false)), Action::Send(Command::LoadOverlay { request_id: 5, path: OVERLAY.into() })]
    );
}

/// Catches a restart loop after `game_exited`, `already_running`, `version_mismatch`,
/// `hook_failed`, `bootstrap_timeout` or a failed start (design: reported once, then `stopped`,
/// no restart).
#[test]
fn the_stop_rules_end_the_host_without_a_restart() {
    for (event, reason) in [
        (Event::GameExited, "game_exited"),
        (Event::error("already_running"), "already_running"),
        (Event::error("version_mismatch"), "version_mismatch"),
        (Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(1428) }, "hook_failed"),
        (Event::error("bootstrap_timeout"), "bootstrap_timeout"),
    ] {
        let mut f = Fake::started();
        assert_eq!(
            f.says(event.clone()),
            vec![emit(event), emit(Event::Stopped { reason: reason.into() }), Action::Stop],
            "{reason}"
        );
        assert_eq!(f.exits(), vec![], "{reason}");
        assert_eq!(f.sup.next_deadline(), None, "{reason}: no restart scheduled");
        assert!(f.sup.is_stopped());
    }

    let mut f = Fake::started();
    f.exits();
    f.at(1_000);
    assert_eq!(
        f.sup.handle(Input::StartFailed("game_not_found".into()), 1_000),
        vec![emit(Event::error("game_not_found")), emit(Event::Stopped { reason: "game_not_found".into() }), Action::Stop]
    );
    assert_eq!(f.sup.next_deadline(), None);
}

/// Catches the other helper failures stopping the host: the design restarts the helper.
#[test]
fn other_helper_errors_are_reported_and_the_helper_restarted() {
    for code in ["no_window", "channel_failed", "pipe_error"] {
        let mut f = Fake::started();
        assert_eq!(f.says(Event::error(code)), vec![emit(Event::error(code))], "{code}");
        f.now = 100;
        f.exits();
        assert_eq!(f.at(1_100), vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&[])], "{code}");
    }
}

/// Catches an unparseable helper line being dropped silently (plan 1 deferred minor).
#[test]
fn a_line_that_is_not_an_event_is_a_helper_protocol_error() {
    let mut f = Fake::started();
    assert_eq!(
        f.sup.handle(Input::HelperLine("wine: something odd".into()), 0),
        vec![emit(Event::HelperProtocolError { line: "wine: something odd".into() })]
    );
}

/// Catches `overlay_timeout` missing, repeated, or raised although the overlay connected.
#[test]
fn status_ok_without_an_overlay_within_10_s_is_overlay_timeout() {
    assert_eq!(OVERLAY_TIMEOUT_MS, 10_000, "the design's 10 s");
    let ok = |id| Event::Status { request_id: id, code: "ok".into(), detail: 0, message: String::new() };
    let mut f = Fake::started();
    f.now = 1_000;
    f.says(ok(1));
    assert_eq!(f.at(1_000 + OVERLAY_TIMEOUT_MS - 1), vec![]);
    assert_eq!(f.at(1_000 + OVERLAY_TIMEOUT_MS), vec![emit(Event::OverlayTimeout)]);
    assert_eq!(f.at(60_000), vec![], "reported once");

    let mut f = Fake::started();
    f.says(ok(1));
    f.now = 5_000;
    f.says(Event::OverlayConnected { module_version: 1 });
    assert_eq!(f.at(30_000), vec![]);
}

#[test]
fn shutdown_stops_quietly() {
    let mut f = Fake::started();
    assert_eq!(f.sup.handle(Input::Shutdown, 0), vec![Action::Stop]);
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), None);
}

fn data_connected(producer_id: u64, from_seq: u64) -> Event {
    Event::DataConnected {
        channel: 1,
        producer_id,
        from_seq,
        schema: vec![SchemaCategory::new("test", &[("ticks", 32)])],
    }
}

fn data_event(seq: u64) -> Event {
    Event::DataEvent { channel: 1, seq, name: "tick".into(), data: seq.to_string(), ts: seq * 1_000 }
}

/// Catches a resume point that does not follow the design (design 2b "uw-host": on
/// `data_connected` the seq becomes `from_seq` and a new producer replaces the old one; on each
/// `data_event` it becomes that event's seq), or one moved by events that carry no seq.
#[test]
fn the_resume_point_follows_data_connected_and_each_data_event() {
    let mut f = Fake::started();
    assert_eq!(f.sup.data_resume(1), None);
    assert_eq!(f.says(data_connected(77, 5)), vec![emit(data_connected(77, 5))]);
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 5 }));
    f.says(data_event(6));
    f.says(data_event(7));
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 7 }));
    f.says(Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: None, ts: 1 });
    f.says(Event::DataLost { channel: 1, events: 3 });
    f.says(Event::DataDisconnected { channel: 1 });
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 7 }));
    // A new producer (a new game session) replaces the old one.
    f.says(data_connected(88, 40));
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 88, seq: 40 }));
    assert_eq!(f.sup.data_resume(2), None);
    // Each channel has its own point: channel 2's events leave channel 1's alone.
    f.says(Event::DataConnected {
        channel: 2,
        producer_id: 99,
        from_seq: 50,
        schema: vec![SchemaCategory::new("test", &[("ticks", 32)])],
    });
    f.says(Event::DataEvent { channel: 2, seq: 51, name: "tick".into(), data: "51".into(), ts: 51_000 });
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 88, seq: 40 }));
    assert_eq!(f.sup.data_resume(2), Some(ResumePoint { producer_id: 99, seq: 51 }));
}

/// Catches a restarted helper started without `--data-resume` (it would start at the write head
/// and lose the events of the downtime), or with a stale point.
#[test]
fn a_restarted_helper_gets_data_resume_with_the_last_seq_delivered() {
    let mut f = Fake::started();
    f.says(data_connected(77, 5));
    f.says(data_event(6));
    f.now = 100;
    f.exits();
    assert_eq!(
        f.at(1_100),
        vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&["--data-resume", "77:6"])]
    );
    f.says(data_connected(77, 6));
    f.says(data_event(7));
    f.now = 2_000;
    f.exits();
    assert_eq!(
        f.at(4_000),
        vec![emit(Event::HelperRestarted { attempt: 2 }), start_helper(&["--data-resume", "77:7"])]
    );
}

/// Catches data events taken for replies (design 2b: they carry no request id, so they never
/// touch pending commands), changed on the way to the user, or answered with reconcile commands.
#[test]
fn data_events_pass_through_and_never_touch_pending_commands() {
    let mut f = Fake::started();
    let ping = Command::Ping { request_id: f.ids.next() };
    f.user(ping);
    for e in [
        data_connected(77, 0),
        data_event(1),
        Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: Some("1".into()), ts: 1 },
        Event::DataLost { channel: 1, events: 2 },
        Event::DataDisconnected { channel: 1 },
    ] {
        assert_eq!(f.says(e.clone()), vec![emit(e)]);
    }
    // The ping is still pending: the helper's death answers it, once.
    assert_eq!(f.exits(), vec![emit(Event::reply_error(1, "overlay_not_connected"))]);
}

/// Catches `data_version_mismatch` or `data_bad_layout` stopping the host (design 2b: "Not stop
/// codes"; the helper reports them and waits for `disconnect`).
#[test]
fn data_errors_are_reported_and_do_not_stop_the_host() {
    for code in ["data_version_mismatch", "data_bad_layout"] {
        let mut f = Fake::started();
        let e = Event::data_error(code, 1);
        assert_eq!(f.says(e.clone()), vec![emit(e)], "{code}");
        assert!(!f.sup.is_stopped(), "{code}");
    }
}
