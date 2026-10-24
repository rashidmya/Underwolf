use uw_pipe_proto::data::{
    FORMAT_VERSION, HELLO_SIZE, Hello, MAX_CATEGORIES, MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN,
    ReadError, Schema, SchemaCategory, SchemaError, SchemaKey, read_hello, write_hello,
};

fn test_schema() -> Schema {
    Schema::new(vec![
        SchemaCategory::new("game_info", &[("phase", 64)]),
        SchemaCategory::new("match_info", &[("match_id", 32), ("roster_0", 1024)]),
    ])
    .unwrap()
}

/// Catches a hello block written at other offsets than the design's table (magic 0, version 4,
/// pid 8, reserved 12, `producer_id` 16), which the producer and the helper would disagree on.
#[test]
fn the_hello_block_sits_at_the_design_offsets() {
    let mut b = [0xAAu8; HELLO_SIZE];
    write_hello(&mut b, 1234, 0x0102_0304_0506_0708);
    assert_eq!(HELLO_SIZE, 24);
    assert_eq!(&b[0..4], b"UWDH");
    assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 1234);
    assert_eq!(u32::from_le_bytes(b[12..16].try_into().unwrap()), 0);
    assert_eq!(
        u64::from_le_bytes(b[16..24].try_into().unwrap()),
        0x0102_0304_0506_0708
    );
    assert_eq!(FORMAT_VERSION, 1);
    assert_eq!(
        read_hello(&b, 1234),
        Ok(Hello {
            pid: 1234,
            producer_id: 0x0102_0304_0506_0708
        })
    );
}

/// Catches the helper reading a hello block it must refuse (design "Failures"): another version is
/// `data_version_mismatch`; a wrong magic, a short block or another process's pid is
/// `data_bad_layout`.
#[test]
fn a_hello_block_that_does_not_check_out_is_refused() {
    let mut b = [0u8; HELLO_SIZE];
    write_hello(&mut b, 1234, 5);
    let mut v2 = b;
    v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(read_hello(&v2, 1234), Err(ReadError::Version(2)));
    assert_eq!(
        read_hello(&v2, 1234).unwrap_err().code(),
        "data_version_mismatch"
    );

    let mut magic = b;
    magic[3] = b'X';
    let e = read_hello(&magic, 1234).unwrap_err();
    assert_eq!(e, ReadError::Layout("hello block magic is not UWDH".into()));
    assert_eq!(e.code(), "data_bad_layout");
    assert_eq!(
        read_hello(&b, 99),
        Err(ReadError::Layout(
            "hello block pid 1234, not the game's 99".into()
        ))
    );
    assert_eq!(
        read_hello(&b[..16], 1234),
        Err(ReadError::Layout(
            "hello block is 16 bytes, under 24".into()
        ))
    );
    // A block that was created but never written (paging-file pages start as 0).
    assert!(matches!(
        read_hello(&[0u8; HELLO_SIZE], 1234),
        Err(ReadError::Layout(_))
    ));
}

/// Catches a serialized schema that differs from the design's form (u32 category count; per
/// category `u16 len, name, u32 key_count`; per key `u16 len, name, u32 max_len`).
#[test]
fn a_schema_serializes_to_the_design_form_and_parses_back() {
    let s = Schema::new(vec![SchemaCategory::new("ab", &[("k", 7)])]).unwrap();
    let mut want = Vec::new();
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&2u16.to_le_bytes());
    want.extend_from_slice(b"ab");
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&1u16.to_le_bytes());
    want.extend_from_slice(b"k");
    want.extend_from_slice(&7u32.to_le_bytes());
    assert_eq!(s.serialize(), want);
    assert_eq!(Schema::parse(&want), Ok(s));

    let t = test_schema();
    assert_eq!(Schema::parse(&t.serialize()), Ok(t));
}

/// Catches keys numbered other than across categories in order (the info store's slots and
/// timestamps are indexed this way on both sides).
#[test]
fn keys_are_numbered_across_categories_in_order() {
    let s = test_schema();
    assert_eq!(s.key_count(), 3);
    assert_eq!(s.key_index("game_info", "phase"), Some(0));
    assert_eq!(s.key_index("match_info", "match_id"), Some(1));
    assert_eq!(s.key_index("match_info", "roster_0"), Some(2));
    assert_eq!(s.key_index("match_info", "phase"), None);
    assert_eq!(s.key_index("nope", "phase"), None);
    let keys: Vec<_> = s.keys().collect();
    assert_eq!(
        keys,
        [
            ("game_info", "phase", 64),
            ("match_info", "match_id", 32),
            ("match_info", "roster_0", 1024)
        ]
    );
    assert_eq!(
        s.categories()[1].keys[0],
        SchemaKey {
            key: "match_id".into(),
            max_len: 32
        }
    );
}

/// Catches the design's limits not being enforced (names of at most 255 bytes, `max_len` of at
/// most 64 KiB, at most 1024 keys) or a name used twice being accepted.
#[test]
fn a_schema_over_the_limits_is_refused() {
    let long = "n".repeat(MAX_NAME_LEN + 1);
    assert_eq!(
        Schema::new(vec![SchemaCategory::new(&long, &[])]),
        Err(SchemaError::NameTooLong(256))
    );
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[(&long, 1)])]),
        Err(SchemaError::NameTooLong(256))
    );
    assert!(Schema::new(vec![SchemaCategory::new(&"n".repeat(MAX_NAME_LEN), &[])]).is_ok());
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[("k", MAX_VALUE_LEN + 1)])]),
        Err(SchemaError::MaxLenTooLarge {
            key: "k".into(),
            max_len: 65537
        })
    );
    assert!(Schema::new(vec![SchemaCategory::new("c", &[("k", MAX_VALUE_LEN)])]).is_ok());
    let names: Vec<String> = (0..=MAX_KEYS).map(|i| format!("k{i}")).collect();
    let keys: Vec<(&str, u32)> = names.iter().map(|n| (n.as_str(), 1)).collect();
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &keys)]),
        Err(SchemaError::TooManyKeys(1025))
    );
    assert!(Schema::new(vec![SchemaCategory::new("c", &keys[..MAX_KEYS])]).is_ok());
    assert_eq!(
        Schema::new(vec![
            SchemaCategory::new("c", &[]),
            SchemaCategory::new("c", &[])
        ]),
        Err(SchemaError::Duplicate("c".into()))
    );
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[("k", 1), ("k", 2)])]),
        Err(SchemaError::Duplicate("c.k".into()))
    );
    // The same key name in two categories is two keys.
    assert!(
        Schema::new(vec![
            SchemaCategory::new("a", &[("k", 1)]),
            SchemaCategory::new("b", &[("k", 1)])
        ])
        .is_ok()
    );
}

/// Catches the helper accepting schema bytes that do not fit: cut short, with bytes left over, a
/// name that is not UTF-8, or over a limit.
#[test]
fn schema_bytes_that_do_not_fit_are_refused() {
    let good = test_schema().serialize();
    assert_eq!(
        Schema::parse(&good[..good.len() - 1]),
        Err(SchemaError::Malformed("the schema ends early"))
    );
    let mut extra = good.clone();
    extra.push(0);
    assert_eq!(
        Schema::parse(&extra),
        Err(SchemaError::Malformed("bytes left over after the schema"))
    );
    let mut bad_utf8 = Schema::new(vec![SchemaCategory::new("ab", &[])])
        .unwrap()
        .serialize();
    bad_utf8[6] = 0xFF; // the second byte of "ab"
    assert_eq!(
        Schema::parse(&bad_utf8),
        Err(SchemaError::Malformed("a schema name is not UTF-8"))
    );
    let mut too_big = Schema::new(vec![SchemaCategory::new("c", &[("k", 1)])])
        .unwrap()
        .serialize();
    let n = too_big.len();
    too_big[n - 4..].copy_from_slice(&(MAX_VALUE_LEN + 1).to_le_bytes());
    assert!(matches!(
        Schema::parse(&too_big),
        Err(SchemaError::MaxLenTooLarge { .. })
    ));
    // A huge category count with nothing behind it is refused before anything is allocated.
    assert_eq!(
        Schema::parse(&u32::MAX.to_le_bytes()),
        Err(SchemaError::TooManyCategories(u32::MAX as usize))
    );
}

/// Catches a schema with more categories than `MAX_CATEGORIES`, or one category with more keys
/// than `MAX_KEYS`, being accepted by `new` or `parse`.
#[test]
fn too_many_categories_or_keys_are_refused() {
    let over = MAX_CATEGORIES + 1;
    let mut bytes = (over as u32).to_le_bytes().to_vec();
    for _ in 0..over {
        bytes.extend_from_slice(&0u16.to_le_bytes()); // an empty name
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no keys
    }
    assert_eq!(
        Schema::parse(&bytes),
        Err(SchemaError::TooManyCategories(over))
    );
    let categories: Vec<SchemaCategory> = (0..over)
        .map(|i| SchemaCategory::new(&format!("c{i}"), &[]))
        .collect();
    assert_eq!(
        Schema::new(categories),
        Err(SchemaError::TooManyCategories(over))
    );

    let over = MAX_KEYS + 1;
    let mut bytes = 1u32.to_le_bytes().to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.push(b'c');
    bytes.extend_from_slice(&(over as u32).to_le_bytes());
    // Bytes for MAX_KEYS keys only: the cap must fire on the count, before the next key is read.
    for _ in 0..MAX_KEYS {
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(b'k');
        bytes.extend_from_slice(&1u32.to_le_bytes());
    }
    assert_eq!(Schema::parse(&bytes), Err(SchemaError::TooManyKeys(over)));
}
