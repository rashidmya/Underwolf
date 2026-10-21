use uw_spike_gep_channel::{ChannelError, DATA_MAX, NAME_MAX, RING_SLOTS, RingReader, ring_init, ring_push, ring_region_size};

fn ring() -> Vec<u8> {
    let mut m = vec![0x55; ring_region_size()];
    ring_init(&mut m);
    m
}

#[test]
fn ring_overwrites_oldest_and_reports_loss() {
    let mut m = ring();
    for i in 0..35u64 {
        ring_push(&mut m, &format!("e{i}"), format!("d{i}").as_bytes(), 1000 + i).unwrap();
    }
    let (recs, lost) = RingReader::new().drain(&m);
    assert_eq!(lost, 5);
    assert_eq!(recs.len(), RING_SLOTS);
    for (n, r) in recs.iter().enumerate() {
        let i = n as u64 + 5;
        assert_eq!(r.name, format!("e{i}"));
        assert_eq!(r.data, format!("d{i}").as_bytes());
        assert_eq!(r.ts, 1000 + i);
    }
}

#[test]
fn drain_returns_only_new_records() {
    let mut m = ring();
    let mut r = RingReader::new();
    assert_eq!(r.drain(&m), (vec![], 0));
    ring_push(&mut m, "a", b"1", 1).unwrap();
    ring_push(&mut m, "b", b"", 2).unwrap();
    let (recs, lost) = r.drain(&m);
    assert_eq!((recs.len(), lost), (2, 0));
    assert_eq!(recs[1].name, "b");
    assert!(recs[1].data.is_empty());
    assert_eq!(r.drain(&m), (vec![], 0));
    ring_push(&mut m, "c", b"3", 3).unwrap();
    assert_eq!(r.drain(&m).0[0].name, "c");
}

#[test]
fn from_tail_skips_history() {
    let mut m = ring();
    ring_push(&mut m, "old", b"", 1).unwrap();
    let mut r = RingReader::from_tail(&m);
    assert!(r.drain(&m).0.is_empty());
    ring_push(&mut m, "new", b"", 2).unwrap();
    assert_eq!(r.drain(&m).0[0].name, "new");
}

#[test]
fn oversize_name_or_data_is_rejected() {
    let mut m = ring();
    assert_eq!(ring_push(&mut m, &"n".repeat(NAME_MAX + 1), b"", 1), Err(ChannelError::TooLong));
    assert_eq!(ring_push(&mut m, "n", &vec![0; DATA_MAX + 1], 1), Err(ChannelError::TooLong));
    ring_push(&mut m, &"n".repeat(NAME_MAX), &vec![7; DATA_MAX], 1).unwrap();
    let (recs, _) = RingReader::new().drain(&m);
    assert_eq!((recs[0].name.len(), recs[0].data.len()), (NAME_MAX, DATA_MAX));
}

#[test]
fn bad_regions_are_rejected() {
    let mut m = vec![0u8; ring_region_size()];
    assert_eq!(ring_push(&mut m, "a", b"", 1), Err(ChannelError::BadMagic));
    ring_init(&mut m);
    let n = m.len();
    assert_eq!(ring_push(&mut m[..n - 1], "a", b"", 1), Err(ChannelError::Truncated));
    assert_eq!(RingReader::new().drain(&[0u8; 8]), (vec![], 0));
}
