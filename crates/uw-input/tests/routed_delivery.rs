use uw_input::{configuration::PreparedConfig, delivery::*, requests::ConfigurationQueue};
use uw_pipe_proto::{input::*, routing::*};

fn target(id: u32) -> WidgetRef {
    WidgetRef { id, generation: 1 }
}
fn input(id: u32, revision: u64, record: InputRecord) -> RoutedEvent {
    RoutedEvent { target: Some(target(id)), revision, kind: EventKind::Input, argument: 0, input: Some(record) }
}
fn motion(id: u32, revision: u64) -> RoutedEvent {
    input(id, revision, InputRecord::motion(id as i32, 3, 0))
}
fn key() -> RoutedEvent {
    input(1, 1, InputRecord::key(65, 30, true, false, false, 0))
}
fn cancel(id: u32) -> RoutedEvent {
    RoutedEvent {
        target: Some(target(id)),
        revision: 1,
        kind: EventKind::Cancel,
        argument: InputReason::Hidden as u32,
        input: None,
    }
}
fn barrier(id: u32) -> RoutingResult {
    RoutingResult { epoch: 1, revision: id as u64, request_id: id, code: RoutingCode::Applied, through_seq: 999 }
}
fn ready() -> RoutedOutbox {
    let q = RoutedOutbox::new();
    q.reset(1);
    q
}
fn batch(q: &RoutedOutbox) -> RoutedBatch {
    let Some(RoutedOutput::Events(b)) = q.take(1) else { panic!("expected events") };
    b
}

#[test]
fn motion_coalesces_only_for_same_instance_and_snapshot() {
    let q = ready();
    assert_eq!(q.try_push(1, motion(1, 1)), PushResult::Queued);
    let moved = input(1, 1, InputRecord::motion(50, 60, 0));
    assert_eq!(q.try_push(1, moved), PushResult::Coalesced);
    assert_eq!(q.try_push(1, motion(2, 1)), PushResult::Queued);
    assert_eq!(q.try_push(1, motion(2, 2)), PushResult::Queued);
    let newer = RoutedEvent { target: Some(WidgetRef { id: 2, generation: 2 }), ..motion(2, 2) };
    assert_eq!(q.try_push(1, newer), PushResult::Queued);
    assert_eq!(batch(&q).events, vec![moved, motion(2, 1), motion(2, 2), newer]);
}

#[test]
fn cancellation_and_configuration_form_an_ordered_sequence_barrier() {
    let q = ready();
    let wheel = input(1, 1, InputRecord::wheel(120, 0, 5, 6, 0));
    for e in [motion(1, 1), key(), wheel] {
        assert_eq!(q.try_push(1, e), PushResult::Queued);
    }
    assert_eq!(q.try_group(1, &[cancel(1)], Some(barrier(2))), PushResult::Queued);
    q.try_push(1, motion(1, 2));
    let before = batch(&q);
    assert_eq!(before.first_seq, 1);
    assert_eq!(before.events, vec![motion(1, 1), key(), wheel, cancel(1)]);
    assert_eq!(q.take(1), Some(RoutedOutput::Configured(RoutingResult { through_seq: 4, ..barrier(2) })));
    let after = batch(&q);
    assert_eq!(after.first_seq, 5);
    assert_eq!(after.events, vec![motion(1, 2)]);
}

#[test]
fn barriers_break_motion_coalescing_even_without_cancellations() {
    let q = ready();
    q.try_push(1, motion(1, 1));
    q.try_group(1, &[], Some(barrier(1)));
    assert_eq!(q.try_push(1, motion(1, 1)), PushResult::Queued);
    assert_eq!(batch(&q).events.len(), 1);
    assert_eq!(q.take(1), Some(RoutedOutput::Configured(RoutingResult { through_seq: 1, ..barrier(1) })));
    assert_eq!(batch(&q).first_seq, 2);
}

#[test]
fn full_input_queue_retains_independent_control_capacity() {
    let q = ready();
    for _ in 0..1024 {
        assert_eq!(q.try_push(1, key()), PushResult::Queued);
    }
    for id in 1..=32 {
        assert_eq!(q.try_group(1, &[cancel(id)], Some(barrier(id))), PushResult::Queued);
    }
    assert!(!q.revoked());
    assert_eq!(q.try_push(1, cancel(1)), PushResult::Revoke);
    assert!(q.revoked());
    let mut events = 0;
    let mut barriers = 0;
    while let Some(out) = q.take(1) {
        match out {
            RoutedOutput::Events(b) => events += b.events.len(),
            RoutedOutput::Configured(_) => barriers += 1,
        }
    }
    assert_eq!((events, barriers), (1056, 32));
}

#[test]
fn overflowing_group_is_never_partially_queued() {
    let q = ready();
    for _ in 0..63 {
        q.try_push(1, cancel(1));
    }
    assert_eq!(q.try_group(1, &[cancel(2)], Some(barrier(2))), PushResult::Revoke);
    assert!(q.revoked());
    assert_eq!(batch(&q).events, vec![cancel(1); 63]);
    assert!(q.take(1).is_none());
}

#[test]
fn only_motion_can_be_dropped_without_revoking() {
    let q = ready();
    for _ in 0..1024 {
        q.try_push(1, key());
    }
    assert_eq!(q.try_push(1, motion(1, 1)), PushResult::DroppedMove);
    assert!(!q.revoked());
    assert_eq!(q.try_push(1, key()), PushResult::Revoke);
}

#[test]
fn stale_resets_and_pushes_cannot_erase_new_stream() {
    let q = ready();
    q.reset(2);
    q.try_push(2, key());
    q.reset(1);
    assert_eq!(q.try_push(1, cancel(1)), PushResult::Stale);
    assert!(q.take(1).is_none());
    let Some(RoutedOutput::Events(b)) = q.take(2) else { panic!("new stream lost") };
    assert_eq!(b.first_seq, 1);
    assert_eq!(b.events, vec![key()]);
    q.reset(3);
    assert!(q.take(3).is_none());
    assert!(!q.revoked());
}

#[test]
fn batches_are_bounded_without_sequence_gaps() {
    let q = ready();
    for _ in 0..130 {
        q.try_push(1, key());
    }
    for (first, len) in [(1, 64), (65, 64), (129, 2)] {
        let b = batch(&q);
        assert_eq!(b.first_seq, first);
        assert_eq!(b.events.len(), len);
    }
}

#[test]
fn invalid_group_does_not_deliver_valid_prefix() {
    let q = ready();
    assert_eq!(q.try_group(1, &[key(), RoutedEvent { revision: 0, ..cancel(1) }], None), PushResult::Revoke);
    assert!(q.revoked());
    assert!(q.take(1).is_none());
}

#[test]
fn configuration_queue_bounds_pending_work_and_keeps_application_order() {
    let q = ConfigurationQueue::new();
    for revision in 1..=33 {
        let c = PreparedConfig::compile(RoutingConfig {
            epoch: 1,
            revision,
            request_id: revision as u32,
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
        })
        .unwrap();
        assert_eq!(q.try_push(c).is_ok(), revision <= 32);
    }
    for revision in 1..=32 {
        assert_eq!(q.pop().unwrap().config().revision, revision);
    }
    assert!(q.pop().is_none());
}
