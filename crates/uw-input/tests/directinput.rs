use uw_input::directinput::{DeviceIntent, DeviceKind, DeviceRegistry};
#[test]
fn successful_unacquire_replaces_previous_acquire_intent() {
    let mut intent = DeviceIntent::default();
    intent.acquired(0);
    assert!(intent.restore_requested());
    intent.unacquired(0);
    assert!(!intent.restore_requested());
}
#[test]
fn failed_acquire_and_unacquire_preserve_the_last_successful_intent() {
    let mut intent = DeviceIntent::default();
    intent.acquired(-1);
    assert!(!intent.restore_requested());
    intent.acquired(0);
    intent.unacquired(-1);
    assert!(intent.restore_requested());
    intent.acquired(-1);
    assert!(intent.restore_requested());
}
#[test]
fn no_effect_unacquire_still_cancels_restore() {
    let mut intent = DeviceIntent::default();
    intent.acquired(1);
    intent.unacquired(1);
    assert!(!intent.restore_requested());
}
#[test]
fn first_forced_unacquire_recovers_preexisting_acquisition_without_inventing_intent() {
    let mut acquired = DeviceIntent::default();
    acquired.isolated(0);
    assert!(acquired.restore_requested());
    assert!(acquired.is_isolated());
    let mut inactive = DeviceIntent::default();
    inactive.isolated(1);
    assert!(!inactive.restore_requested());
    let mut failed = DeviceIntent::default();
    failed.isolated(-1);
    assert!(!failed.is_isolated());
}
#[test]
fn repeated_isolation_preserves_game_intent_until_explicit_unacquire() {
    let mut intent = DeviceIntent::default();
    intent.isolated(0);
    intent.isolated(1);
    assert!(intent.restore_requested());
    intent.unacquired(1);
    intent.isolated(1);
    assert!(!intent.restore_requested());
}

#[test]
fn forced_isolation_cannot_overwrite_a_newer_explicit_unacquire() {
    let mut intent = DeviceIntent::default();
    intent.acquired(0);
    intent.unacquired(0);
    intent.isolated(0);
    assert!(!intent.restore_requested());
}

#[test]
fn registry_applies_native_results_to_current_game_intent() {
    let registry = DeviceRegistry::new();
    let token = registry.observe(100, DeviceKind::Keyboard).unwrap();
    registry.apply(token, |intent| intent.acquired(0)).unwrap();
    registry.apply(token, |intent| intent.unacquired(0)).unwrap();
    registry.apply(token, |intent| intent.isolated(0)).unwrap();
    assert!(!registry.intent(token).unwrap().restore_requested());
}
#[test]
fn held_quarantine_and_failed_reacquisition_keep_the_device_isolated() {
    let mut intent = DeviceIntent::default();
    intent.isolated(0);
    assert!(!intent.may_restore(true));
    assert!(intent.may_restore(false));
    intent.restored(-1);
    assert!(intent.is_isolated());
    intent.restored(0);
    assert!(!intent.is_isolated());
    assert!(!intent.may_restore(false));
}

#[test]
fn registry_lifetimes_cannot_transfer_intent_or_retire_a_reused_identity() {
    let registry = DeviceRegistry::new();
    let old = registry.observe(100, DeviceKind::Keyboard).unwrap();
    let mut intent = registry.intent(old).unwrap();
    intent.acquired(0);
    registry.update(old, intent).unwrap();
    assert!(registry.intent(old).unwrap().restore_requested());
    let fresh = registry.created(100, DeviceKind::Keyboard).unwrap();
    registry.retire(old);
    assert!(registry.intent(old).is_none());
    assert!(!registry.intent(fresh).unwrap().restore_requested());
    registry.retire(fresh);
    assert!(registry.find(100).is_none());
}
#[test]
fn native_type_scope_does_not_claim_controller_isolation() {
    assert_eq!(DeviceKind::from_native_type(0x113), DeviceKind::Keyboard);
    assert_eq!(DeviceKind::from_native_type(0x112), DeviceKind::Mouse);
    assert_eq!(DeviceKind::from_native_type(0x114), DeviceKind::Other);
}
#[test]
fn bounded_registry_rejects_exhaustion_and_reuses_only_retired_slots() {
    let registry = DeviceRegistry::new();
    let mut first = None;
    for identity in 1..=256 {
        let token = registry.observe(identity, DeviceKind::Keyboard).unwrap();
        if identity == 1 {
            first = Some(token);
        }
    }
    assert!(registry.observe(257, DeviceKind::Mouse).is_err());
    registry.retire(first.unwrap());
    assert!(registry.observe(257, DeviceKind::Mouse).is_ok());
}
