use uw_pipe_proto::jsonl::{Command, Event};

#[test]
fn load_overlay_command_round_trips() {
    let c = Command::LoadOverlay { path: r"Z:\games\overlay.dll".into() };
    let line = c.to_line();
    assert_eq!(line, r#"{"cmd":"load_overlay","path":"Z:\\games\\overlay.dll"}"#);
    assert_eq!(Command::parse(&line).unwrap(), c);
}

#[test]
fn events_round_trip() {
    for e in [
        Event::BootstrapConnected { overlay_loaded: false },
        Event::BootstrapConnected { overlay_loaded: true },
        Event::Status { code: "ok".into(), detail: 0, message: "overlay started".into() },
        Event::Status { code: "load_failed".into(), detail: 126, message: "LoadLibraryW failed".into() },
        Event::error("no_window"),
        Event::Error { code: "hook_failed".into(), detail: Some(1455) },
    ] {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
    }
}

#[test]
fn error_without_detail_omits_the_field() {
    assert_eq!(Event::error("no_window").to_line(), r#"{"event":"error","code":"no_window"}"#);
}

#[test]
fn status_carries_detail_and_message() {
    let line = Event::Status { code: "start_failed".into(), detail: 7, message: "nonzero".into() }.to_line();
    assert_eq!(line, r#"{"event":"status","code":"start_failed","detail":7,"message":"nonzero"}"#);
}

#[test]
fn unknown_command_is_an_error() {
    assert!(Command::parse(r#"{"cmd":"nope"}"#).is_err());
    assert!(Command::parse("not json").is_err());
}
