use uw_overlay::*;
#[test]
fn start_rejects_invalid_params() {
    assert_eq!(validate_params(8, 1, 5, 5), Err(StartError::Short));
    assert_eq!(validate_params(12, 2, 5, 5), Err(StartError::Version));
    assert_eq!(validate_params(12, 1, 5, 6), Err(StartError::Pid));
    assert_eq!(validate_params(12, 1, 5, 5), Ok(()));
}
#[test]
fn start_is_idempotent() {
    let once = StartOnce::default();
    assert!(once.claim());
    assert!(!once.claim());
}
