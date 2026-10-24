use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::{Command, Event};

#[test]
fn load_overlay_command_round_trips() {
    let c = Command::LoadOverlay { request_id: 1, path: r"Z:\games\overlay.dll".into() };
    let line = c.to_line();
    assert_eq!(line, r#"{"cmd":"load_overlay","request_id":1,"path":"Z:\\games\\overlay.dll"}"#);
    assert_eq!(Command::parse(&line).unwrap(), c);
}

/// Catches a renamed command or field: these are the wire names in the design's table.
#[test]
fn set_state_and_ping_have_the_design_wire_form() {
    let c = Command::SetState { request_id: 2, edit_mode: Some(true), visible: None };
    assert_eq!(c.to_line(), r#"{"cmd":"set_state","request_id":2,"edit_mode":true}"#);
    assert_eq!(Command::parse(&c.to_line()).unwrap(), c);
    assert_eq!(
        Command::parse(r#"{"cmd":"set_state","request_id":3,"visible":false}"#).unwrap(),
        Command::SetState { request_id: 3, edit_mode: None, visible: Some(false) }
    );
    assert_eq!(Command::Ping { request_id: 4 }.to_line(), r#"{"cmd":"ping","request_id":4}"#);
    assert_eq!(Command::Ping { request_id: 4 }.request_id(), 4);
}

/// Catches the helper accepting a command without an id (its reply could not be matched).
#[test]
fn a_command_without_a_request_id_is_rejected_by_parse() {
    assert!(Command::parse(r#"{"cmd":"ping"}"#).is_err());
}

/// Catches `uw-inject` sending a user's id on to the helper (it could collide with an id the host
/// assigned), losing it (the reply could not carry it back), or accepting one that is not a u32.
#[test]
fn parse_with_id_gives_the_host_id_and_returns_the_users() {
    assert_eq!(Command::parse_with_id(r#"{"cmd":"ping"}"#, 41).unwrap(), (Command::Ping { request_id: 41 }, None));
    assert_eq!(
        Command::parse_with_id(r#"{"cmd":"ping","request_id":7}"#, 41).unwrap(),
        (Command::Ping { request_id: 41 }, Some(7))
    );
    assert!(Command::parse_with_id(r#"{"cmd":"ping","request_id":-1}"#, 41).is_err());
    assert!(Command::parse_with_id("not json", 41).is_err());
}

/// Catches a reply whose id cannot be read or replaced, or an unsolicited event taken for a reply.
#[test]
fn replies_expose_their_request_id() {
    let mut e = Event::Pong { request_id: 3, seen_seq: 0 };
    *e.request_id_mut().unwrap() = 9;
    assert_eq!(e, Event::Pong { request_id: 9, seen_seq: 0 });
    let mut e = Event::reply_error(4, "state_busy");
    *e.request_id_mut().unwrap() = 8;
    assert_eq!(e, Event::reply_error(8, "state_busy"));
    assert_eq!(Event::StateSet { request_id: 5, seq: 1 }.request_id(), Some(5));
    assert_eq!(Event::Pong { request_id: 3, seen_seq: 0 }.request_id(), Some(3));
    assert_eq!(Event::reply_error(4, "state_busy").request_id(), Some(4));
    let status = Event::Status { request_id: 6, code: "ok".into(), detail: 0, message: String::new() };
    assert_eq!(status.request_id(), Some(6));
    assert_eq!(Event::error("no_window").request_id(), None);
    assert!(Event::error("no_window").request_id_mut().is_none());
    assert_eq!(Event::GameExited.request_id(), None);
}

#[test]
fn events_round_trip() {
    for e in [
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None },
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\g\overlay.dll".into()) },
        Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: "overlay started".into() },
        Event::Status { request_id: 2, code: "load_failed".into(), detail: 126, message: "LoadLibraryW failed".into() },
        Event::StateSet { request_id: 3, seq: 9 },
        Event::Pong { request_id: 4, seen_seq: 9 },
        Event::OverlayConnected { module_version: 1 },
        Event::OverlayDisconnected,
        Event::Bell { bits: vec!["state_applied".into(), "bit5".into()], applied_seq: 9 },
        Event::BootstrapDisconnected,
        Event::GameExited,
        Event::error("no_window"),
        Event::reply_error(5, "overlay_not_connected"),
        Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(1455) },
        Event::HelperRestarted { attempt: 2 },
        Event::HelperProtocolError { line: "garbage".into() },
        Event::OverlayTimeout,
        Event::Stopped { reason: "game_exited".into() },
    ] {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
    }
}

/// Catches renamed events or fields: these are the wire names in the design.
#[test]
fn events_have_the_design_wire_form() {
    assert_eq!(Event::error("no_window").to_line(), r#"{"event":"error","code":"no_window"}"#);
    assert_eq!(
        Event::reply_error(5, "overlay_not_connected").to_line(),
        r#"{"event":"error","request_id":5,"code":"overlay_not_connected"}"#
    );
    assert_eq!(
        Event::Status { request_id: 3, code: "start_failed".into(), detail: 7, message: "nonzero".into() }.to_line(),
        r#"{"event":"status","request_id":3,"code":"start_failed","detail":7,"message":"nonzero"}"#
    );
    assert_eq!(
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\o.dll".into()) }.to_line(),
        r#"{"event":"bootstrap_connected","overlay_loaded":true,"overlay_path":"Z:\\o.dll"}"#
    );
    assert_eq!(
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }.to_line(),
        r#"{"event":"bootstrap_connected","overlay_loaded":false}"#
    );
    assert_eq!(
        Event::Bell { bits: vec!["state_applied".into()], applied_seq: 2 }.to_line(),
        r#"{"event":"bell","bits":["state_applied"],"applied_seq":2}"#
    );
    assert_eq!(Event::GameExited.to_line(), r#"{"event":"game_exited"}"#);
    assert_eq!(Event::StateSet { request_id: 1, seq: 2 }.to_line(), r#"{"event":"state_set","request_id":1,"seq":2}"#);
    assert_eq!(Event::Pong { request_id: 1, seen_seq: 2 }.to_line(), r#"{"event":"pong","request_id":1,"seen_seq":2}"#);
}

#[test]
fn unknown_command_is_an_error() {
    assert!(Command::parse(r#"{"cmd":"nope","request_id":1}"#).is_err());
    assert!(Command::parse("not json").is_err());
}

fn data_events() -> Vec<Event> {
    vec![
        Event::DataConnected {
            channel: 1,
            producer_id: 123_456,
            from_seq: 7,
            schema: vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])],
        },
        Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: Some("3".into()), ts: 40 },
        Event::Info { channel: 1, category: "test".into(), key: "last_seq".into(), value: None, ts: 41 },
        Event::DataEvent { channel: 1, seq: 8, name: "tick".into(), data: "3".into(), ts: 42 },
        Event::DataLost { channel: 1, events: 12 },
        Event::DataDisconnected { channel: 1 },
        Event::data_error("data_bad_layout", 1),
    ]
}

/// Catches renamed data events or fields, or a null value left out instead of sent as `null`:
/// these are the wire forms in the design's "New JSON events" table.
#[test]
fn data_events_have_the_design_wire_form() {
    let lines: Vec<String> = data_events().iter().map(Event::to_line).collect();
    assert_eq!(
        lines,
        [
            r#"{"event":"data_connected","channel":1,"producer_id":123456,"from_seq":7,"schema":[{"category":"test","keys":[{"key":"ticks","max_len":32},{"key":"last_seq","max_len":32}]}]}"#,
            r#"{"event":"info","channel":1,"category":"test","key":"ticks","value":"3","ts":40}"#,
            r#"{"event":"info","channel":1,"category":"test","key":"last_seq","value":null,"ts":41}"#,
            r#"{"event":"data_event","channel":1,"seq":8,"name":"tick","data":"3","ts":42}"#,
            r#"{"event":"data_lost","channel":1,"events":12}"#,
            r#"{"event":"data_disconnected","channel":1}"#,
            r#"{"event":"error","code":"data_bad_layout","detail":1}"#,
        ]
    );
}

/// Catches a data event that does not parse back, or one taken for a reply (design: data events
/// carry no `request_id`, so they never touch pending commands).
#[test]
fn data_events_round_trip_and_are_not_replies() {
    for e in data_events() {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
        assert_eq!(e.request_id(), None, "{e:?}");
        assert!(e.clone().request_id_mut().is_none(), "{e:?}");
    }
}
