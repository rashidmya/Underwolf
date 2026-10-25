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

#[test]
fn routing_session_requires_v2_and_rejects_legacy_input() {
    let mut session = RoutingSession::default();
    assert!(session.receive(OverlayMessage::Input(InputMessage::Welcome { input_version: 1 })).is_err());
    assert!(!session.connected());
    assert!(matches!(
        session.receive(OverlayMessage::Routing(uw_pipe_proto::routing::RoutingMessage::Welcome { input_version: 2 })),
        Ok(SessionEvent::Connected)
    ));
    assert!(
        session
            .receive(OverlayMessage::Input(InputMessage::Ack(InputAck { epoch: 1, nonce: 1, through_seq: 0 })))
            .is_err()
    );
    assert!(matches!(session.disconnect(), SessionEvent::Disconnected));
    assert!(!session.connected());
}

#[test]
fn routing_session_delivers_validated_tables_and_fresh_requests() {
    use uw_pipe_proto::routing::*;
    let mut session = RoutingSession::default();
    session.receive(OverlayMessage::Routing(RoutingMessage::Welcome { input_version: 2 })).unwrap();
    let mut config = RoutingConfig {
        epoch: 1,
        revision: 1,
        request_id: 1,
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
    assert!(
        matches!(session.receive(OverlayMessage::Routing(RoutingMessage::Configure(config.clone()))), Ok(SessionEvent::Configure(c)) if c == config)
    );
    config.hotkeys.clear();
    assert!(session.receive(OverlayMessage::Routing(RoutingMessage::Configure(config))).is_err());
    let request = RoutingRequest {
        epoch: 1,
        revision: 1,
        request_id: 2,
        target: None,
        action: RoutingAction::EnterEdit,
        state_seq: None,
    };
    assert!(
        matches!(session.receive(OverlayMessage::Routing(RoutingMessage::Request(request))), Ok(SessionEvent::RouteRequest(r)) if r == request)
    );
    assert_eq!(session.seen_seq(), 0);
    session.applied(10);
    assert_eq!(session.seen_seq(), 10);
}

#[test]
fn source_stamped_off_coalesces_only_with_a_newer_hidden_state() {
    use uw_pipe_proto::routing::*;
    let request = RoutingRequest {
        epoch: 1,
        revision: 1,
        request_id: 7,
        target: None,
        action: RoutingAction::LeaveEdit,
        state_seq: Some(3),
    };
    let hidden = RoutingSnapshot::new(request, 4, false);
    assert_eq!(hidden.request, RoutingRequest { state_seq: Some(4), ..request });
    assert!(hidden.defer_visibility);
    let visible = RoutingSnapshot::new(request, 4, true);
    assert_eq!(visible.request.state_seq, Some(3));
    assert!(!visible.defer_visibility);
    let future = RoutingSnapshot::new(RoutingRequest { state_seq: Some(99), ..request }, 4, false);
    assert_eq!(future.request.state_seq, Some(99));
    assert!(!future.defer_visibility);
    let on = RoutingSnapshot::new(RoutingRequest { action: RoutingAction::EnterEdit, ..request }, 4, false);
    assert_eq!(on.request.state_seq, Some(3));
    assert!(!on.defer_visibility);
}
