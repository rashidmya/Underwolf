use uw_pipe_proto::data::{
    INFO_HEADER_SIZE, InfoLayout, InfoReader, InfoValue, ReadError, Schema, SchemaCategory,
    info_init, info_set,
};

fn schema() -> Schema {
    Schema::new(vec![
        SchemaCategory::new("game", &[("phase", 5)]),
        SchemaCategory::new("match", &[("id", 12), ("score", 9)]),
    ])
    .unwrap()
}

/// A formatted store for `schema()`, its layout and the region.
fn store() -> (Schema, InfoLayout, Vec<u8>) {
    let s = schema();
    let layout = InfoLayout::of(&s);
    let mut region = vec![0xEEu8; layout.size()];
    info_init(&mut region, &s, &layout);
    (s, layout, region)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn value(category: &str, key: &str, v: Option<&str>, ts: u64) -> InfoValue {
    InfoValue {
        category: category.into(),
        key: key.into(),
        value: v.map(str::to_string),
        ts,
    }
}

/// Catches a store laid out differently from the design: the 32-byte header (magic 0, version 4,
/// `schema_len` 8, `key_count` 12, `slot_area_off` 16), the schema right after it, one 8-aligned
/// u64 timestamp per key, then one slot per key (`len` u32 plus `max_len` bytes, padded to 8,
/// null as `u32::MAX`).
#[test]
fn the_store_sits_at_the_design_offsets() {
    let (s, layout, region) = store();
    let schema_bytes = s.serialize();
    assert_eq!(INFO_HEADER_SIZE, 32);
    assert_eq!(&region[0..4], b"UWDI");
    assert_eq!(u32_at(&region, 4), 1);
    assert_eq!(u32_at(&region, 8) as usize, schema_bytes.len());
    assert_eq!(u32_at(&region, 12), 3);
    assert_eq!(&region[20..32], &[0u8; 12], "reserved bytes");
    assert_eq!(&region[32..32 + schema_bytes.len()], &schema_bytes[..]);
    let ts_off = (32 + schema_bytes.len() + 7) & !7;
    let slot_area = ts_off + 3 * 8;
    assert_eq!(u32_at(&region, 16) as usize, slot_area);
    assert_eq!(
        &region[ts_off..slot_area],
        &[0u8; 24],
        "every key starts never set"
    );
    // Slots: phase (4 + 5 -> 16), id (4 + 12 -> 16), score (4 + 9 -> 16).
    for slot in [slot_area, slot_area + 16, slot_area + 32] {
        assert_eq!(u32_at(&region, slot), u32::MAX, "every key starts null");
    }
    assert_eq!(layout.size(), slot_area + 48);
    assert_eq!(
        (layout.max_len(0), layout.max_len(1), layout.max_len(2)),
        (5, 12, 9)
    );
}

/// Catches the snapshot missing a key that was set, holding one that never was, or losing the
/// difference between null and an empty value.
#[test]
fn the_snapshot_holds_every_key_that_was_set_with_nulls() {
    let (_, layout, mut region) = store();
    assert!(info_set(&mut region, &layout, 0, Some("Lobby"), 10));
    assert!(
        info_set(&mut region, &layout, 2, None, 11),
        "a never-set key changes even to null"
    );
    let mut r = InfoReader::attach(&region).unwrap();
    assert_eq!(r.schema(), &schema());
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [
            value("game", "phase", Some("Lobby"), 10),
            value("match", "score", None, 11)
        ]
    );
    // Null and empty are different values.
    assert!(info_set(&mut region, &layout, 2, Some(""), 12));
    assert!(info_set(&mut region, &layout, 2, None, 13));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "score", None, 13)]
    );
    assert!(info_set(&mut region, &layout, 2, Some(""), 14));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "score", Some(""), 14)]
    );
}

/// Catches a timestamp that goes backwards or stays put on a change (the helper would miss the
/// change), or an unchanged value that moves its timestamp (the helper would report a change).
#[test]
fn timestamps_only_move_forward_and_an_unchanged_value_keeps_its_own() {
    let (_, layout, mut region) = store();
    let mut r = InfoReader::attach(&region).unwrap();
    assert!(info_set(&mut region, &layout, 1, Some("m1"), 100));
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("match", "id", Some("m1"), 100)]
    );
    // The producer's clock went back: the timestamp still moves forward.
    assert!(info_set(&mut region, &layout, 1, Some("m2"), 50));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "id", Some("m2"), 101)]
    );
    // The same value again, later: nothing changes, not even the timestamp.
    assert!(!info_set(&mut region, &layout, 1, Some("m2"), 500));
    assert_eq!(r.changes(&region).unwrap(), []);
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("match", "id", Some("m2"), 101)]
    );
    // A value at its max_len fits.
    assert!(info_set(&mut region, &layout, 1, Some("123456789012"), 600));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "id", Some("123456789012"), 600)]
    );
    // An unchanged null keeps its timestamp too.
    assert!(info_set(&mut region, &layout, 2, None, 700));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "score", None, 700)]
    );
    assert!(!info_set(&mut region, &layout, 2, None, 800));
    assert_eq!(r.changes(&region).unwrap(), []);
}

/// Catches `info_set` writing a value longer than its slot (it would run into the next slot).
#[test]
#[should_panic(expected = "info_set: a value over its max_len")]
fn info_set_refuses_a_value_over_its_max_len() {
    let (_, layout, mut region) = store();
    info_set(&mut region, &layout, 1, Some("1234567890123"), 1);
}

/// Catches `changes` reporting a key twice, or one that did not move since the last read.
#[test]
fn changes_report_only_the_keys_that_moved() {
    let (_, layout, mut region) = store();
    info_set(&mut region, &layout, 0, Some("A"), 1);
    info_set(&mut region, &layout, 1, Some("x"), 1);
    let mut r = InfoReader::attach(&region).unwrap();
    assert_eq!(r.snapshot(&region).unwrap().len(), 2);
    assert_eq!(r.changes(&region).unwrap(), []);
    info_set(&mut region, &layout, 1, Some("y"), 2);
    info_set(&mut region, &layout, 2, Some("3-1"), 2);
    assert_eq!(
        r.changes(&region).unwrap(),
        [
            value("match", "id", Some("y"), 2),
            value("match", "score", Some("3-1"), 2)
        ]
    );
    assert_eq!(r.changes(&region).unwrap(), []);
}

/// Catches the helper attaching to a store whose header does not fit (design "Failures"):
/// another version is `data_version_mismatch`; a wrong magic, a schema that does not parse or
/// runs past the store, a `key_count` or `slot_area_off` that disagrees with the schema, or a
/// store too small for its schema is `data_bad_layout`.
#[test]
fn a_store_whose_header_does_not_fit_is_refused() {
    let (_, _, good) = store();
    let layout_err = |r: &[u8]| match InfoReader::attach(r) {
        Err(ReadError::Layout(why)) => why,
        other => panic!("expected a layout error, got {:?}", other.map(|_| ())),
    };
    let mut b = good.clone();
    b[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(InfoReader::attach(&b).err(), Some(ReadError::Version(2)));

    let mut b = good.clone();
    b[0] = b'X';
    assert_eq!(layout_err(&b), "info store magic is not UWDI");
    let mut b = good.clone();
    b[8..12].copy_from_slice(&100_000u32.to_le_bytes());
    assert_eq!(
        layout_err(&b),
        "info schema of 100000 bytes runs past the store"
    );
    let mut b = good.clone();
    let n = u32_at(&b, 8);
    b[8..12].copy_from_slice(&(n - 1).to_le_bytes());
    assert_eq!(layout_err(&b), "info schema: the schema ends early");
    let mut b = good.clone();
    b[12..16].copy_from_slice(&4u32.to_le_bytes());
    assert_eq!(
        layout_err(&b),
        "info key_count 4, but the schema has 3 keys"
    );
    let mut b = good.clone();
    let off = u32_at(&b, 16);
    b[16..20].copy_from_slice(&(off + 8).to_le_bytes());
    assert_eq!(
        layout_err(&b),
        format!(
            "info slot_area_off {}, but the schema puts it at {off}",
            off + 8
        )
    );
    assert_eq!(
        layout_err(&good[..good.len() - 1]),
        format!(
            "info store is {} bytes, its schema needs {}",
            good.len() - 1,
            good.len()
        )
    );
    assert_eq!(
        layout_err(&good[..16]),
        "info store is 16 bytes, under its header"
    );
    // A mapping that was created but never formatted (paging-file pages start as 0).
    assert_eq!(
        layout_err(&vec![0u8; good.len()]),
        "info store magic is not UWDI"
    );
}

/// Catches a read that trusts what it finds (design: "Every read is checked"): a slot length over
/// its `max_len`, a value that is not UTF-8, or a header changed after attach (for example by an
/// owner that died holding the lock, design "WAIT_ABANDONED") must be `data_bad_layout`, and a
/// version changed after attach `data_version_mismatch`.
#[test]
fn every_read_checks_the_header_and_each_slot() {
    let (_, layout, mut region) = store();
    let slot_area = u32_at(&region, 16) as usize;
    info_set(&mut region, &layout, 0, Some("Lobby"), 1);
    let mut r = InfoReader::attach(&region).unwrap();

    let mut b = region.clone();
    b[slot_area..slot_area + 4].copy_from_slice(&6u32.to_le_bytes());
    assert_eq!(
        r.snapshot(&b),
        Err(ReadError::Layout(
            "game.phase: length 6 over its max_len 5".into()
        ))
    );
    let mut b = region.clone();
    b[slot_area + 4] = 0xFF;
    assert_eq!(
        r.snapshot(&b),
        Err(ReadError::Layout("game.phase: value is not UTF-8".into()))
    );
    let mut b = region.clone();
    b[0] = b'X';
    assert_eq!(
        r.changes(&b),
        Err(ReadError::Layout("info store magic is not UWDI".into()))
    );
    let mut b = region.clone();
    b[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(r.changes(&b), Err(ReadError::Version(2)));
    let mut b = region.clone();
    b[20] = 1; // a reserved byte: still a different header from the one attached
    assert_eq!(
        r.changes(&b),
        Err(ReadError::Layout(
            "info header changed under the same producer".into()
        ))
    );
    let mut b = region.clone();
    b[32 + 4 + 2] ^= 1; // a byte of a schema name: the schema still parses, but is not the one attached
    assert_eq!(
        r.changes(&b),
        Err(ReadError::Layout(
            "info header changed under the same producer".into()
        ))
    );
    // The good store still reads.
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("game", "phase", Some("Lobby"), 1)]
    );
}
