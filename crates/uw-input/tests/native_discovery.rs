use uw_input::native::{AdapterCoverage, DiscoveryError, DiscoveryStatus, SignatureId, TargetRegistry};

#[test]
fn compatible_shared_implementation_is_registered_once() {
    let mut targets = TargetRegistry::new();
    assert!(targets.insert(0x1000, SignatureId(1)).unwrap());
    assert!(!targets.insert(0x1000, SignatureId(1)).unwrap());
}

#[test]
fn incompatible_shared_address_preserves_original_registration() {
    let mut targets = TargetRegistry::new();
    targets.insert(0x1000, SignatureId(1)).unwrap();
    assert_eq!(targets.insert(0x1000, SignatureId(2)), Err(DiscoveryError::IncompatibleAlias));
    assert!(!targets.insert(0x1000, SignatureId(1)).unwrap());
}

#[test]
fn null_target_is_never_registered_for_hooking() {
    assert_eq!(TargetRegistry::new().insert(0, SignatureId(1)), Err(DiscoveryError::NullTarget));
}

#[test]
fn full_registry_still_accepts_an_existing_target_but_rejects_growth() {
    let mut targets = TargetRegistry::new();
    for address in 1..=256 {
        assert!(targets.insert(address, SignatureId(1)).unwrap());
    }
    assert!(!targets.insert(1, SignatureId(1)).unwrap());
    assert_eq!(targets.insert(257, SignatureId(1)), Err(DiscoveryError::Capacity));
}

#[test]
fn failed_alias_does_not_consume_capacity() {
    let mut targets = TargetRegistry::new();
    targets.insert(1, SignatureId(1)).unwrap();
    assert_eq!(targets.insert(1, SignatureId(2)), Err(DiscoveryError::IncompatibleAlias));
    for address in 2..=256 {
        assert!(targets.insert(address, SignatureId(1)).unwrap());
    }
}

#[test]
fn unimplemented_runtime_is_distinguished_from_a_failed_native_call() {
    let missing = AdapterCoverage::failure(0x8000_4001u32 as i32);
    assert_eq!(missing.status, DiscoveryStatus::Unsupported);
    assert_eq!(missing.abi_mask, 0);
    assert_eq!(missing.detail, 0x8000_4001u32 as i32);
    assert_eq!(AdapterCoverage::failure(0x8000_4005u32 as i32).status, DiscoveryStatus::Failed);
}
