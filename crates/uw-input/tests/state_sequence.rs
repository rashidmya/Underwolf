use uw_input::requests::StateSequence;
#[test]
fn requests_cannot_invent_observed_state_or_lower_the_latest_sequence() {
    let sequence = StateSequence::default();
    assert!(!sequence.is_current(Some(99)));
    sequence.observe(2);
    sequence.observe(1);
    assert!(sequence.is_current(Some(2)));
    assert!(sequence.not_future(Some(1)));
    assert!(!sequence.not_future(Some(99)));
    assert!(!sequence.is_current(Some(1)));
    assert!(sequence.is_current(None));
}
#[test]
fn a_new_observation_invalidates_an_inflight_native_publication() {
    let sequence = StateSequence::default();
    sequence.observe(1);
    let applying = sequence.begin(Some(1));
    assert!(sequence.publication_allowed());
    assert!(sequence.observe(2));
    assert!(!sequence.publication_allowed());
    drop(applying);
    assert!(sequence.publication_allowed());
}
#[test]
fn reentrant_ui_work_cannot_override_a_stale_outer_transaction() {
    let sequence = StateSequence::default();
    sequence.observe(1);
    let outer = sequence.begin(Some(1));
    sequence.observe(2);
    {
        let _nested = sequence.begin(Some(2));
        assert!(!sequence.publication_allowed());
    }
    assert!(!sequence.publication_allowed());
    drop(outer);
    let _fresh = sequence.begin(Some(2));
    assert!(sequence.publication_allowed());
}
