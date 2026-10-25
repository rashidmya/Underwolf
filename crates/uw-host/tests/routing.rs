use uw_host::routing::{RoutingReceiver, RoutingReceiverAction};
use uw_pipe_proto::{
    input::{InputChallenge, InputReason},
    jsonl::{Event, RoutingEvent},
    routing::*,
};
fn state(epoch: u64) -> Event {
    Event::Routing(RoutingEvent::RoutingState {
        state: RoutingState {
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
        },
    })
}
fn batch(epoch: u64) -> Event {
    Event::Routing(RoutingEvent::RoutedBatch {
        batch: RoutedBatch {
            epoch,
            first_seq: 1,
            events: vec![RoutedEvent { revision: 1, target: None, kind: EventKind::Hotkey, argument: 1, input: None }],
        },
    })
}
fn ready() -> RoutingReceiver {
    let mut receiver = RoutingReceiver::default();
    receiver.event(&state(1)).unwrap();
    receiver.event(&batch(1)).unwrap();
    receiver
}
#[test]
fn challenge_waits_for_application_consumption() {
    let mut receiver = ready();
    let c = InputChallenge { epoch: 1, nonce: 1, through_seq: 1 };
    assert!(receiver.acknowledge(c).is_none());
    receiver.consumed(1).unwrap();
    assert!(receiver.acknowledge(c).is_some());
    assert!(receiver.acknowledge(InputChallenge { nonce: 0, ..c }).is_none());
    assert!(receiver.acknowledge(InputChallenge { through_seq: 2, ..c }).is_none());
}
#[test]
fn configuration_retirement_waits_for_preceding_cancellation_consumption() {
    let mut receiver = ready();
    let result = RoutingResult { epoch: 1, revision: 2, request_id: 7, code: RoutingCode::Applied, through_seq: 1 };
    assert!(matches!(
        receiver.event(&Event::Routing(RoutingEvent::RoutingConfigured { result })),
        Ok(RoutingReceiverAction::Deferred)
    ));
    assert!(matches!(receiver.consumed(1).unwrap().as_slice(), [RoutingReceiverAction::Configured(r)] if *r == result));
    assert!(receiver.consumed(2).is_err());
}
#[test]
fn safety_disconnect_and_new_epoch_drop_old_delivery_and_barriers() {
    let mut receiver = ready();
    receiver.event(&Event::OverlayDisconnected).unwrap();
    assert!(matches!(receiver.event(&state(1)), Ok(RoutingReceiverAction::Ignore)));
    assert!(matches!(receiver.event(&state(2)), Ok(RoutingReceiverAction::Reset(_))));
    assert!(matches!(receiver.event(&batch(1)), Ok(RoutingReceiverAction::Ignore)));
    assert!(receiver.acknowledge(InputChallenge { epoch: 1, nonce: 1, through_seq: 0 }).is_none());
}
#[test]
fn queued_barriers_keep_wire_order_and_have_separate_bounded_capacity() {
    let mut receiver = ready();
    for request_id in 1..=32 {
        let result = RoutingResult { epoch: 1, revision: 2, request_id, code: RoutingCode::Applied, through_seq: 1 };
        receiver.event(&Event::Routing(RoutingEvent::RoutingConfigured { result })).unwrap();
        receiver.event(&Event::Routing(RoutingEvent::RoutingResult { result })).unwrap();
    }
    let result = RoutingResult { epoch: 1, revision: 2, request_id: 33, code: RoutingCode::Applied, through_seq: 1 };
    assert!(receiver.event(&Event::Routing(RoutingEvent::RoutingConfigured { result })).is_err());
    let actions = receiver.consumed(1).unwrap();
    assert_eq!(actions.len(), 64);
    assert!(matches!(actions.as_slice(), [RoutingReceiverAction::Configured(_), RoutingReceiverAction::Result(_), ..]));
}

fn config() -> RoutingConfig {
    RoutingConfig {
        epoch: 1,
        revision: 7,
        request_id: 42,
        width: 100,
        height: 100,
        widgets: vec![WidgetSpec {
            instance: WidgetRef { id: 1, generation: 1 },
            x: 0,
            y: 0,
            width: 80,
            height: 80,
            z: 1,
            mode: InputMode::Block,
            visible: true,
            keyboard: true,
        }],
        hotkeys: vec![HotkeySpec {
            id: 1,
            vk: 9,
            modifiers: 8,
            flags: 0,
            action: HotkeyAction::ToggleEdit,
            target: None,
        }],
    }
}
#[test]
fn desired_tables_are_republished_with_fresh_correlation_and_no_ownership_replay() {
    use uw_host::{
        RequestIds,
        driver::{Desired, Driver},
    };
    use uw_pipe_proto::jsonl::{Command, RoutingCommand};
    let mut driver = Driver::new(Desired::new(None), RequestIds::default());
    driver.note_command(&Command::Routing(RoutingCommand::ConfigureInput { request_id: 42, config: config() }));
    driver.on_event(&Event::Routing(RoutingEvent::RoutingReady { version: 2 }));
    assert!(driver.input_capable());
    let first = driver.on_event(&state(1));
    let [Command::Routing(RoutingCommand::ConfigureInput { request_id, config: first })] = first.as_slice() else {
        panic!("desired table not published: {first:?}");
    };
    assert_eq!(*request_id, first.request_id);
    assert_ne!(first.request_id, 42);
    assert_eq!(first.epoch, 1);
    assert!(driver.on_event(&state(1)).is_empty());
    driver.note_command(&Command::Routing(RoutingCommand::RouteRequest {
        request_id: 90,
        request: RoutingRequest {
            epoch: 1,
            revision: first.revision,
            request_id: 90,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        },
    }));
    driver.on_event(&Event::OverlayDisconnected);
    assert!(driver.on_event(&state(1)).is_empty(), "old session must not republish");
    let next = driver.on_event(&state(2));
    let [Command::Routing(RoutingCommand::ConfigureInput { config: next, .. })] = next.as_slice() else {
        panic!("reconnect replayed ownership: {next:?}");
    };
    assert_eq!(next.epoch, 2);
    assert!(next.revision > first.revision);
    assert_eq!(next.widgets, config().widgets);
    assert_eq!(next.hotkeys, config().hotkeys);
    assert!(!driver.desired().edit_mode);
}
#[test]
fn invalid_desired_table_does_not_replace_the_last_valid_configuration() {
    use uw_host::{
        RequestIds,
        driver::{Desired, Driver},
    };
    use uw_pipe_proto::jsonl::{Command, RoutingCommand};
    let mut driver = Driver::new(Desired::new(None), RequestIds::default());
    driver.note_command(&Command::Routing(RoutingCommand::ConfigureInput { request_id: 42, config: config() }));
    let mut invalid = config();
    invalid.hotkeys.clear();
    invalid.width = 99;
    driver.note_command(&Command::Routing(RoutingCommand::ConfigureInput { request_id: 42, config: invalid }));
    let commands = driver.on_event(&state(1));
    assert!(
        matches!(commands.as_slice(), [Command::Routing(RoutingCommand::ConfigureInput { config, .. })] if config.width == 100)
    );
}

#[test]
fn mismatched_application_replies_do_not_clear_supervised_commands() {
    use uw_host::{
        RequestIds,
        driver::Desired,
        supervisor::{Action, Input, Supervisor},
    };
    use uw_pipe_proto::jsonl::{Command, RoutingCommand};
    for wrong in [0, 1, 2] {
        let mut supervisor = Supervisor::new(Desired::new(None), RequestIds::default());
        supervisor.start(0);
        let mut config = config();
        config.request_id = 7;
        supervisor.handle(Input::User(Command::Routing(RoutingCommand::ConfigureInput { request_id: 7, config })), 1);
        let result = RoutingResult {
            epoch: if wrong == 2 { 2 } else { 1 },
            revision: if wrong == 1 { 8 } else { 7 },
            request_id: 7,
            code: RoutingCode::Applied,
            through_seq: 0,
        };
        let event = if wrong == 0 {
            RoutingEvent::RoutingResult { result }
        } else {
            RoutingEvent::RoutingConfigured { result }
        };
        supervisor.handle(Input::HelperLine(Event::Routing(event).to_line()), 2);
        let loss = supervisor.handle(Input::HelperExited, 3);
        assert!(
            loss.iter().any(|action| matches!(action, Action::Emit(Event::Error { request_id: Some(7), .. }))),
            "wrong result cleared exact pending correlation"
        );
    }
}
#[test]
fn host_bounds_configuration_and_request_applications_independently() {
    use uw_host::{
        RequestIds,
        driver::Desired,
        supervisor::{Action, Input, Supervisor},
    };
    use uw_pipe_proto::jsonl::{Command, RoutingCommand};
    let mut supervisor = Supervisor::new(Desired::new(None), RequestIds::default());
    supervisor.start(0);
    for request_id in 1..=32 {
        let mut config = config();
        config.request_id = request_id;
        assert!(
            supervisor
                .handle(Input::User(Command::Routing(RoutingCommand::ConfigureInput { request_id, config })), 1)
                .iter()
                .any(|action| matches!(action, Action::Send(_)))
        );
        let request = RoutingRequest {
            epoch: 1,
            revision: 1,
            request_id: request_id + 100,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        };
        assert!(
            supervisor
                .handle(
                    Input::User(Command::Routing(RoutingCommand::RouteRequest {
                        request_id: request.request_id,
                        request
                    })),
                    1
                )
                .iter()
                .any(|action| matches!(action, Action::Send(_)))
        );
    }
    let mut config = config();
    config.request_id = 33;
    let actions =
        supervisor.handle(Input::User(Command::Routing(RoutingCommand::ConfigureInput { request_id: 33, config })), 2);
    assert!(
        matches!(actions.as_slice(), [Action::Emit(Event::Error { request_id: Some(33), code, .. })] if code == "routing_requests_full")
    );
}
#[test]
fn a_busy_table_publication_retries_on_a_fresh_challenge_without_replaying_ownership() {
    use uw_host::{
        RequestIds,
        driver::{Desired, Driver},
    };
    use uw_pipe_proto::jsonl::{Command, RoutingCommand};
    let mut driver = Driver::new(Desired::new(None), RequestIds::default());
    driver.note_command(&Command::Routing(RoutingCommand::ConfigureInput { request_id: 42, config: config() }));
    let first = driver.on_event(&state(1));
    let first_id = first[0].request_id();
    assert!(driver.on_event(&Event::reply_error(first_id, "routing_requests_full")).is_empty());
    let commands = driver.on_event(&Event::Routing(RoutingEvent::RoutingChallenge {
        challenge: InputChallenge { epoch: 1, nonce: 1, through_seq: 0 }.into(),
    }));
    assert!(
        matches!(commands.as_slice(), [Command::Routing(RoutingCommand::ConfigureInput { request_id, .. })] if *request_id != first_id)
    );
}
