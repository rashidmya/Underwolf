use uw_input::delivery::*;
use uw_pipe_proto::input::*;
fn ack(c: InputChallenge) -> InputAck {
    InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq }
}
#[test]
fn discrete_overflow_revokes() {
    let q = Outbox::new();
    q.reset(1);
    for _ in 0..1024 {
        assert_eq!(q.try_push(1, InputRecord::key(65, 30, true, false, false, 0)), PushResult::Queued)
    }
    assert_eq!(q.try_push(1, InputRecord::key(65, 30, false, false, false, 0)), PushResult::Revoke);
    assert!(q.revoked());
    assert_eq!(q.take_batch(1).unwrap().records.len(), 64);
}
#[test]
fn moves_do_not_cross_button_barrier() {
    let q = Outbox::new();
    q.reset(1);
    q.try_push(1, InputRecord::motion(1, 2, 0));
    assert_eq!(q.try_push(1, InputRecord::motion(3, 4, 0)), PushResult::Coalesced);
    q.try_push(1, InputRecord::button(0, true, 3, 4, 0));
    q.try_push(1, InputRecord::motion(5, 6, 0));
    let b = q.take_batch(1).unwrap();
    assert_eq!(b.first_seq, 1);
    assert_eq!(
        b.records,
        vec![InputRecord::motion(3, 4, 0), InputRecord::button(0, true, 3, 4, 0), InputRecord::motion(5, 6, 0)]
    );
    q.try_push(1, InputRecord::motion(7, 8, 0));
    assert_eq!(q.take_batch(1).unwrap().first_seq, 4);
}
#[test]
fn heartbeat_cannot_skip_input() {
    let mut l = Lease::new(1);
    let c = l.challenge(0, 4).unwrap();
    assert!(!l.ack(InputAck { through_seq: 3, ..ack(c) }, 10));
    assert!(!l.ack(InputAck { nonce: 99, ..ack(c) }, 10));
    assert!(l.ack(ack(c), 10));
    assert!(!l.ack(ack(c), 11));
    assert!(l.healthy(499));
    assert!(!l.healthy(500));
}
#[test]
fn delayed_ack_cannot_extend_lease() {
    let mut l = Lease::new(1);
    let a = l.challenge(0, 0).unwrap();
    let b = l.challenge(100, 0).unwrap();
    assert!(l.ack(ack(b), 200));
    assert!(!l.ack(ack(a), 300));
    assert!(l.healthy(599));
    assert!(!l.healthy(600));
    let c = l.challenge(600, 0).unwrap();
    assert!(!l.ack(ack(c), 1100));
}
#[test]
fn old_epoch_is_discarded() {
    let q = Outbox::new();
    q.reset(1);
    q.try_push(1, InputRecord::text('a', 0));
    q.reset(2);
    assert!(q.take_batch(2).is_none());
    assert_eq!(q.try_push(1, InputRecord::text('b', 0)), PushResult::Stale);
    assert!(!q.revoked());
    assert!(q.take_batch(1).is_none());
    let mut l = Lease::new(1);
    let c = l.challenge(0, 0).unwrap();
    l.reset(2);
    assert!(!l.ack(ack(c), 1));
    assert!(!l.healthy(1));
}
#[test]
fn healthy_idle_receiver_keeps_lease_and_challenges_are_bounded() {
    let mut l = Lease::new(1);
    for now in (0..2000).step_by(100) {
        let c = l.challenge(now, 0).unwrap();
        assert!(l.ack(ack(c), now + 1));
        assert!(l.healthy(now + 99));
        assert!(l.challenge(now + 99, 0).is_none());
    }
    let mut stalled = Lease::new(1);
    for now in (0..10000).step_by(100) {
        stalled.challenge(now, 0);
        assert!(stalled.outstanding() <= 8);
    }
    assert!(!stalled.healthy(10000));
}
#[test]
fn late_old_batch_cannot_publish_progress_into_a_new_epoch() {
    let mut progress = SentProgress::default();
    progress.announce(1);
    assert!(progress.sent(1, 17));
    // UI epoch changed before the old worker completion; no new epoch batch was sent.
    assert_eq!(progress.through(2), 0);
    progress.announce(2);
    assert!(!progress.sent(1, 18));
    assert_eq!(progress.through(2), 0);
    assert!(progress.sent(2, 3));
    progress.announce(1);
    assert_eq!(progress.through(2), 3);
}
