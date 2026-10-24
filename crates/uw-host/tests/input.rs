use uw_host::{
    RequestIds,
    driver::{Desired, Driver},
    input::*,
};
use uw_pipe_proto::{
    input::*,
    jsonl::{Command, Event},
};
fn state(epoch: u64, active: bool) -> Event {
    Event::InputState {
        state: InputState { epoch, active, reason: InputReason::Ready, state_seq: None, width: 800, height: 600 },
    }
}
#[test]
fn ack_requires_consumed_records() {
    let mut r = ReceiverState::default();
    r.event(&state(1, true)).unwrap();
    r.event(&Event::InputBatch {
        batch: InputBatch { epoch: 1, first_seq: 1, records: vec![InputRecord::text('a', 0)] },
    })
    .unwrap();
    let c = InputChallenge { epoch: 1, nonce: 1, through_seq: 1 };
    assert_eq!(r.acknowledge(c), None);
    r.consumed(1).unwrap();
    assert_eq!(r.acknowledge(c), Some(InputAck { epoch: 1, nonce: 1, through_seq: 1 }));
    r.event(&state(2, false)).unwrap();
    assert_eq!(r.acknowledge(c), None);
}
#[test]
fn input_ready_selects_capture_protocol() {
    let mut d = Driver::new(Desired::new(None), RequestIds::default());
    d.on_event(&Event::InputReady { version: 1 });
    let cmd = d.on_event(&Event::OverlayConnected { module_version: 2 });
    assert!(matches!(cmd.as_slice(), [Command::SetState { edit_mode: None, .. }]));
}
#[test]
fn disconnected_edit_on_is_not_saved_for_input_module() {
    let mut d = Driver::new(Desired::new(None), RequestIds::default());
    d.on_event(&Event::InputReady { version: 1 });
    d.note_command(&Command::SetState { input_epoch: Some(1), request_id: 1, edit_mode: Some(true), visible: None });
    assert!(!d.desired().edit_mode);
    d.on_event(&state(1, true));
    assert!(d.desired().edit_mode);
    d.on_event(&Event::OverlayDisconnected);
    assert!(!d.desired().edit_mode);
}
#[test]
fn hotkey_state_does_not_echo() {
    let mut d = Driver::new(Desired::new(None), RequestIds::default());
    d.on_event(&Event::InputReady { version: 1 });
    assert!(d.on_event(&state(1, true)).is_empty());
    assert!(d.desired().edit_mode);
}
#[test]
fn old_epochs_and_sequence_gaps_never_deliver() {
    let mut r = ReceiverState::default();
    r.event(&state(2, false)).unwrap();
    let b = InputBatch { epoch: 1, first_seq: 1, records: vec![InputRecord::text('x', 0)] };
    assert!(matches!(r.event(&Event::InputBatch { batch: b }), Ok(ReceiverAction::Ignore)));
    assert!(
        r.event(&Event::InputBatch {
            batch: InputBatch { epoch: 2, first_seq: 2, records: vec![InputRecord::text('x', 0)] }
        })
        .is_err()
    );
}
#[test]
fn disconnected_receiver_cannot_acknowledge_old_idle_challenge() {
    let mut r = ReceiverState::default();
    r.event(&state(2, true)).unwrap();
    let c = InputChallenge { epoch: 2, nonce: 1, through_seq: 0 };
    r.event(&Event::OverlayDisconnected).unwrap();
    assert_eq!(r.acknowledge(c), None);
    assert!(matches!(r.event(&state(2, true)), Ok(ReceiverAction::Ignore)));
    r.event(&state(3, false)).unwrap();
    assert!(r.acknowledge(InputChallenge { epoch: 3, ..c }).is_some());
}

#[test]
fn helper_loss_announces_input_disconnect_after_accepted_request() {
    use uw_host::supervisor::{Action, Input, Supervisor};
    let mut s = Supervisor::new(Desired::new(None), RequestIds::default());
    s.start(0);
    s.handle(Input::HelperLine(Event::InputReady { version: 1 }.to_line()), 1);
    s.handle(Input::HelperLine(Event::OverlayConnected { module_version: 2 }.to_line()), 2);
    let actions = s.handle(Input::HelperExited, 3);
    assert!(actions.iter().any(|a| matches!(a, Action::Emit(Event::OverlayDisconnected))));
}
