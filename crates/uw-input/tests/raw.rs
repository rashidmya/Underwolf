use uw_input::raw::*;
use uw_input::registration::*;
fn record(kind: u32, size: usize) -> Vec<u8> {
    let mut b = vec![0; size];
    b[..4].copy_from_slice(&kind.to_le_bytes());
    b[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    b
}
#[test]
fn buffer_walk_preserves_hid_and_counts() {
    let mut mouse = record(0, 48);
    mouse[28..44].fill(1);
    let mut keyboard = record(1, 40);
    keyboard[24..40].fill(2);
    let mut hid = record(2, 37);
    hid[24..].fill(3);
    let mut bytes = mouse;
    bytes.extend(keyboard);
    bytes.extend(&hid);
    let spans: Vec<_> = RawRecordIter::new(&bytes, 3).collect::<Result<_, _>>().unwrap();
    assert_eq!(spans, vec![0..48, 48..88, 88..125]);
    for span in spans {
        neutralize(&mut bytes[span], &mut ReleaseSet::default()).unwrap()
    }
    assert_eq!(&bytes[28..44], &[0; 16]);
    assert_eq!(&bytes[88..], hid.as_slice());
    assert_eq!(u16::from_le_bytes(bytes[78..80].try_into().unwrap()), 255);
}
#[test]
fn malformed_buffers_are_rejected_without_out_of_bounds_reads() {
    for bytes in [vec![0; 23], record(0, 24), record(1, 30)] {
        assert!(RawRecordIter::new(&bytes, 1).collect::<Result<Vec<_>, _>>().is_err());
    }
    assert!(RawRecordIter::new(&record(0, 48), 2).collect::<Result<Vec<_>, _>>().is_err());
}
#[test]
fn tagged_releases_survive_neutralization() {
    let mut b = record(0, 48);
    b[28..30].copy_from_slice(&2u16.to_le_bytes());
    b[44..48].copy_from_slice(&RELEASE_TAG.to_le_bytes());
    let original = b.clone();
    let mut releases = ReleaseSet::default();
    releases.mouse_up(0);
    neutralize(&mut b, &mut releases).unwrap();
    assert_eq!(b, original);
}
#[test]
fn registration_latest_success_wins() {
    let initial = Registration { page: 1, usage: 2, flags: 0x230, target: 123 };
    let mut ledger = RegistrationLedger::new(&[initial]);
    let captured = ledger.capture_descriptors();
    assert_eq!(captured, vec![Registration { flags: 0, target: 123, ..initial }]);
    let next = Registration { flags: 0x30, target: 456, ..initial };
    ledger.record_success(&[next]);
    assert_eq!(ledger.restore_descriptors(), vec![next]);
    let removed = Registration { flags: 1, target: 0, ..initial };
    ledger.record_success(&[removed]);
    assert_eq!(ledger.restore_descriptors(), vec![removed]);
    let hid = Registration { page: 12, usage: 1, flags: 0, target: 555 };
    ledger.record_success(&[hid]);
    assert!(!ledger.capture_descriptors().iter().any(|r| r.page == 12));
}

#[test]
fn pending_release_is_single_use_and_does_not_allow_motion() {
    let mut pending = ReleaseSet::default();
    pending.mouse_up(0);
    let mut first = record(0, 48);
    first[28..30].copy_from_slice(&2u16.to_le_bytes());
    first[36..40].copy_from_slice(&123i32.to_le_bytes());
    first[44..48].copy_from_slice(&RELEASE_TAG.to_le_bytes());
    let mut replay = first.clone();
    neutralize(&mut first, &mut pending).unwrap();
    assert_eq!(&first[28..30], &2u16.to_le_bytes());
    assert_eq!(&first[32..44], &[0; 12]);
    neutralize(&mut replay, &mut pending).unwrap();
    assert_eq!(&replay[28..44], &[0; 16]);
}
