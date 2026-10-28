use uw_frame_proto::*;
fn key(generation: u64) -> AllocationKey {
    AllocationKey {
        session: SessionId([1; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 1,
        },
        generation,
    }
}
fn layout() -> FrameLayout {
    FrameLayout {
        width: 16,
        height: 8,
        stride: 64,
        pixel_len: 4096,
        alignment: 4096,
    }
}
fn r(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
fn baseline() -> FrameState {
    let mut s = FrameState::new(key(1), layout());
    s.publish(1, r(0, 0, 16, 8)).unwrap();
    let c = s.claim().unwrap().unwrap();
    s.complete(c).unwrap();
    s
}
#[test]
fn first_publication_requires_full_pixels_and_remains_full_until_consumed() {
    let mut s = FrameState::new(key(1), layout());
    assert_eq!(s.claim().unwrap(), None);
    assert!(s.publish(1, r(1, 1, 2, 2)).is_err());
    s.publish(1, r(0, 0, 16, 8)).unwrap();
    s.publish(2, r(1, 1, 2, 2)).unwrap();
    let c = s.claim().unwrap().unwrap();
    assert_eq!(
        c,
        Claim {
            key: key(1),
            seq: 2,
            damage: r(0, 0, 16, 8)
        }
    );
    assert_eq!(s.snapshot().consumed, 0);
    s.complete(c).unwrap();
    assert!(s.snapshot().baseline);
    assert_eq!(s.snapshot().consumed, 2);
    assert_eq!(s.snapshot().damage, None);
}
#[test]
fn reading_rejects_writes_and_stale_completion_cannot_clear_the_next_update() {
    let mut s = baseline();
    s.publish(2, r(1, 1, 2, 2)).unwrap();
    let c = s.claim().unwrap().unwrap();
    assert_eq!(s.publish(3, r(2, 2, 2, 2)), Err(FrameError::Busy));
    assert_eq!(s.snapshot().consumed, 1);
    s.complete(c).unwrap();
    s.publish(3, r(9, 4, 2, 2)).unwrap();
    assert_eq!(s.complete(c), Err(FrameError::Stale));
    assert_eq!(s.claim().unwrap().unwrap().damage, r(9, 4, 2, 2));
}
#[test]
fn superseded_ready_updates_union_and_cancel_keeps_unconsumed_damage() {
    let mut s = baseline();
    s.publish(2, r(1, 1, 2, 2)).unwrap();
    s.publish(7, r(9, 4, 2, 2)).unwrap();
    let c = s.claim().unwrap().unwrap();
    assert_eq!(c.damage, r(1, 1, 10, 5));
    s.cancel_unsubmitted(c).unwrap();
    assert_eq!(s.snapshot().consumed, 1);
    assert_eq!(s.claim().unwrap().unwrap(), c);
}
#[test]
fn duplicate_zero_and_wrapped_sequences_never_replace_a_publication() {
    let mut s = baseline();
    for seq in [0, 1] {
        assert_eq!(s.publish(seq, r(1, 1, 2, 2)), Err(FrameError::Stale));
    }
    s.publish(u64::MAX, r(1, 1, 2, 2)).unwrap();
    assert_eq!(s.publish(1, r(3, 3, 1, 1)), Err(FrameError::Stale));
    assert_eq!(s.snapshot().produced, u64::MAX);
}
#[test]
fn old_allocation_claims_and_invalid_metadata_are_rejected() {
    let mut s = baseline();
    s.publish(2, r(1, 1, 2, 2)).unwrap();
    let c = s.claim().unwrap().unwrap();
    let mut next = FrameState::new(key(2), layout());
    next.publish(1, r(0, 0, 16, 8)).unwrap();
    let _ = next.claim().unwrap();
    assert_eq!(next.complete(c), Err(FrameError::Stale));
    s.invalidate();
    assert_eq!(s.publish(3, r(0, 0, 16, 8)), Err(FrameError::Invalid));
    assert!(s.claim().is_err());
    let mut bad = FrameState::new(key(1), layout());
    for damage in [r(0, 0, 0, 1), r(15, 0, 2, 1), r(u32::MAX, 0, 2, 1)] {
        assert!(bad.publish(1, damage).is_err());
    }
}
#[test]
fn serialized_state_rejects_impossible_readers_flags_and_reserved_bits() {
    let mut s = baseline();
    s.publish(2, r(1, 1, 2, 2)).unwrap();
    let _ = s.claim().unwrap();
    let b = s.encode();
    assert_eq!(FrameState::decode(key(1), layout(), &b).unwrap(), s);
    for (offset, value) in [(40, 99), (44, 2), (52, 1)] {
        let mut broken = b;
        broken[offset] = value;
        assert!(
            FrameState::decode(key(1), layout(), &broken).is_err(),
            "{offset}"
        );
    }
    let mut broken = b;
    broken[16..24].copy_from_slice(&9u64.to_le_bytes());
    assert!(FrameState::decode(key(1), layout(), &broken).is_err());
    for len in [0, 63, 65] {
        assert!(FrameState::decode(key(1), layout(), &vec![0; len]).is_err());
    }
}
