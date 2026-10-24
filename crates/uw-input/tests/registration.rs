use uw_input::registration::*;

#[test]
fn overlapping_success_invalidates_restore_until_latest_intent_is_replayed() {
    let journal = RegistrationJournal::new();
    let old = Registration { page: 1, usage: 6, flags: 0x30, target: 12 };
    let latest = Registration { flags: 0x100, target: 24, ..old };
    let mut ledger = RegistrationLedger::new(&[old]);
    let token = journal.begin_owner().unwrap();
    let call = journal.begin_call();
    assert!(call.interfered());
    call.success(latest);
    drop(call);
    assert!(!journal.commit_owner(token));
    let repair = journal.begin_owner().unwrap();
    journal.merge(&mut ledger);
    assert_eq!(ledger.restore_descriptors(), vec![latest]);
    // An API removal arriving during the repair also prevents a stale commit.
    let call = journal.begin_call();
    call.success(Registration { flags: 1, target: 0, ..latest });
    drop(call);
    assert!(!journal.commit_owner(repair));
    let final_pass = journal.begin_owner().unwrap();
    journal.merge(&mut ledger);
    assert_eq!(ledger.restore_descriptors()[0].flags, 1);
    assert_eq!(ledger.restore_descriptors()[0].target, 0);
    assert!(journal.commit_owner(final_pass));
}

#[test]
fn failed_registration_does_not_change_successful_intent() {
    let journal = RegistrationJournal::new();
    let r = Registration { page: 1, usage: 2, flags: 0x230, target: 32 };
    journal.begin_call().success(r);
    let failed = journal.begin_call();
    assert!(journal.begin_owner().is_none());
    drop(failed);
    let token = journal.begin_owner().unwrap();
    let mut ledger = RegistrationLedger::new(&[]);
    journal.merge(&mut ledger);
    assert_eq!(ledger.restore_descriptors(), vec![r]);
    assert!(journal.commit_owner(token));
}

#[test]
fn a_new_capture_uses_current_os_snapshot_not_previous_window_intent() {
    let journal = RegistrationJournal::new();
    let old = Registration { page: 1, usage: 6, flags: 0x30, target: 12 };
    journal.begin_call().success(old);
    let token = journal.begin_owner().unwrap();
    let baseline = journal.checkpoint();
    assert!(journal.current(token));
    // The OS removed a registration whose window was destroyed.
    let mut ledger = RegistrationLedger::new(&[]);
    journal.merge_since(&mut ledger, baseline);
    assert!(ledger.restore_descriptors().is_empty());
    let call = journal.begin_call();
    call.success(Registration { target: 24, ..old });
    drop(call);
    assert!(!journal.commit_owner(token));
    let repair = journal.begin_owner().unwrap();
    journal.merge_since(&mut ledger, baseline);
    assert_eq!(ledger.restore_descriptors()[0].target, 24);
    assert!(journal.commit_owner(repair));
}

#[test]
fn registration_phase_is_sampled_after_announcing_the_game_call() {
    let journal = RegistrationJournal::new();
    let setup = journal.begin_owner().unwrap();
    // A reentrant game call during cursor/held-input setup invalidates entry.
    let reentrant = journal.begin_call();
    assert!(reentrant.interfered());
    assert!(!reentrant.captured());
    drop(reentrant);
    assert!(!journal.commit_capture(setup));
    let setup = journal.begin_owner().unwrap();
    assert!(journal.commit_capture(setup));
    let captured = journal.begin_call();
    assert!(captured.captured());
    drop(captured);
    let restoring = journal.begin_owner().unwrap();
    assert!(journal.commit_owner(restoring));
    // A caller resumed after restoration sees passthrough, regardless of earlier capture state.
    assert!(!journal.begin_call().captured());
}
