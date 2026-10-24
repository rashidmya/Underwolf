use uw_game_channel::input::*;
use uw_pipe_proto::{input::*, pipe::OverlayMessage};
#[test]
fn no_capture_before_welcome() {
    let mut s = Session::default();
    let r = CaptureRequest { epoch: 1, state_seq: 4, request_id: 9, on: true };
    assert!(s.receive(OverlayMessage::Input(InputMessage::Capture(r))).is_err());
    assert!(matches!(
        s.receive(OverlayMessage::Input(InputMessage::Welcome { input_version: 1 })),
        Ok(SessionEvent::Connected)
    ));
    assert!(matches!(s.receive(OverlayMessage::Input(InputMessage::Capture(r))), Ok(SessionEvent::Capture(_))));
    assert!(s.receive(OverlayMessage::Input(InputMessage::Welcome { input_version: 1 })).is_err());
}
#[test]
fn disconnect_is_reported_before_retry() {
    let mut s = Session::default();
    s.receive(OverlayMessage::Input(InputMessage::Welcome { input_version: 1 })).unwrap();
    assert!(matches!(s.disconnect(), SessionEvent::Disconnected));
    assert!(!s.connected());
}
#[test]
fn applied_waits_for_ui() {
    let mut s = Session::default();
    s.receive(OverlayMessage::Input(InputMessage::Welcome { input_version: 1 })).unwrap();
    s.receive(OverlayMessage::Input(InputMessage::Capture(CaptureRequest {
        epoch: 1,
        state_seq: 4,
        request_id: 9,
        on: true,
    })))
    .unwrap();
    assert_eq!(s.seen_seq(), 0);
    s.applied(4);
    assert_eq!(s.seen_seq(), 4);
    s.applied(2);
    assert_eq!(s.seen_seq(), 4);
}
