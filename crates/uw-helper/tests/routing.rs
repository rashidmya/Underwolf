use uw_helper::routing::{Purpose, RoutingPending, RoutingRelay};
use uw_pipe_proto::{
    input::{InputChallenge, InputReason},
    jsonl::{Event, RoutingEvent},
    routing::*,
};
fn state(epoch: u64) -> RoutingState {
    RoutingState {
        epoch,
        revision: 1,
        state_seq: None,
        width: 100,
        height: 100,
        pointer: None,
        focus: None,
        takeover: None,
        exclusive: false,
        domains: Domains::default(),
        reason: InputReason::Ready,
    }
}
fn batch(epoch: u64, first_seq: u64) -> RoutingMessage {
    RoutingMessage::Events(RoutedBatch {
        epoch,
        first_seq,
        events: vec![RoutedEvent { revision: 1, target: None, kind: EventKind::Hotkey, argument: 1, input: None }],
    })
}
#[test]
fn relay_requires_epoch_and_contiguous_input_progress() {
    let mut relay = RoutingRelay::default();
    assert!(relay.accept(batch(1, 1)).is_err());
    relay.accept(RoutingMessage::State(state(1))).unwrap();
    assert!(matches!(relay.accept(batch(1, 1)), Ok(Event::Routing(RoutingEvent::RoutedBatch { .. }))));
    assert!(relay.accept(batch(1, 1)).is_err());
    assert!(relay.accept(batch(1, 3)).is_err());
    relay.accept(RoutingMessage::State(state(2))).unwrap();
    assert!(relay.accept(batch(1, 2)).is_err());
    assert!(relay.accept(batch(2, 1)).is_ok());
}
#[test]
fn relay_rejects_a_result_or_challenge_ahead_of_delivery() {
    let mut relay = RoutingRelay::default();
    relay.accept(RoutingMessage::State(state(1))).unwrap();
    let result = RoutingResult { epoch: 1, revision: 2, request_id: 7, code: RoutingCode::Applied, through_seq: 1 };
    assert!(relay.accept(RoutingMessage::Configured(result)).is_err());
    assert!(relay.accept(RoutingMessage::Challenge(InputChallenge { epoch: 1, nonce: 1, through_seq: 1 })).is_err());
    relay.accept(batch(1, 1)).unwrap();
    assert!(relay.accept(RoutingMessage::Configured(result)).is_ok());
}
#[test]
fn exact_application_results_include_purpose_epoch_revision_and_id() {
    let mut pending = RoutingPending::default();
    pending.add(Purpose::Configure, 1, 2, 7, 100).unwrap();
    pending.add(Purpose::Request, 1, 2, 7, 100).unwrap();
    let result = RoutingResult { epoch: 1, revision: 2, request_id: 7, code: RoutingCode::Applied, through_seq: 0 };
    assert!(!pending.take(Purpose::Configure, &RoutingResult { epoch: 2, ..result }));
    assert!(!pending.take(Purpose::Configure, &RoutingResult { revision: 3, ..result }));
    assert!(pending.take(Purpose::Request, &result));
    assert!(!pending.take(Purpose::Request, &result));
    assert!(pending.take(Purpose::Configure, &result));
}
#[test]
fn applications_have_independent_bounds_and_five_second_deadlines() {
    let mut pending = RoutingPending::default();
    for id in 1..=32 {
        pending.add(Purpose::Configure, 1, id as u64, id, 100).unwrap();
    }
    assert!(pending.add(Purpose::Configure, 1, 33, 33, 100).is_err());
    pending.add(Purpose::Request, 1, 1, 33, 101).unwrap();
    assert!(pending.expire(5099).is_empty());
    assert_eq!(pending.expire(5100).len(), 32);
    assert_eq!(pending.drain(), vec![(Purpose::Request, 33)]);
    assert!(pending.add(Purpose::Configure, 1, 1, 1, u64::MAX).is_err());
}

#[test]
fn production_handshake_requires_v2_but_preserves_state_only_overlay() {
    use uw_helper::routing::{First, classify_first};
    use uw_pipe_proto::{
        input::InputMessage,
        pipe::{OverlayMessage, encode_input, encode_overlay},
    };
    let routing =
        OverlayMessage::Routing(RoutingMessage::Hello { module_version: 3, state_layout: 1, input_version: 2 });
    assert_eq!(classify_first(&encode_input(&routing).unwrap()), First::Routing(3));
    let legacy = OverlayMessage::Input(InputMessage::Hello { module_version: 2, state_layout: 1, input_version: 1 });
    assert_eq!(classify_first(&encode_input(&legacy).unwrap()), First::VersionMismatch);
    let state = OverlayMessage::Hello { module_version: 1, state_layout: 1 };
    assert_eq!(classify_first(&encode_overlay(&state)), First::State(1));
}

#[test]
fn epoch_reset_reports_old_applications_lost_without_retiring_new_scope() {
    let mut pending = RoutingPending::default();
    pending.add(Purpose::Configure, 1, 1, 1, 0).unwrap();
    pending.add(Purpose::Request, 1, 1, 2, 0).unwrap();
    pending.add(Purpose::Configure, 2, 2, 3, 0).unwrap();
    assert_eq!(pending.retire_before(2), vec![(Purpose::Configure, 1), (Purpose::Request, 2)]);
    assert_eq!(pending.drain(), vec![(Purpose::Configure, 3)]);
}
