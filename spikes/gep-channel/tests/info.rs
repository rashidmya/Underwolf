use uw_spike_gep_channel::{ChannelError, InfoReader, InfoWriter, Schema, info_region_size};

fn schema() -> Schema {
    Schema {
        categories: vec![
            ("game_info".into(), vec![("phase".into(), 64), ("map".into(), 32)]),
            ("match_info".into(), vec![("roster_0".into(), 2048)]),
        ],
    }
}

fn region(s: &Schema) -> Vec<u8> {
    vec![0xAA; info_region_size(s)]
}

#[test]
fn txn_commits_once_and_skips_unchanged() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"GameInProgress"), 100).unwrap();
    assert!(t.commit());
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"GameInProgress"), 200).unwrap();
    assert!(!t.commit());
    drop(w);

    let r = InfoReader::attach(&mem).unwrap();
    let snap = r.snapshot(&mem);
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].ts, 100);
    assert_eq!(snap[0].value.as_deref(), Some(&b"GameInProgress"[..]));
}

#[test]
fn changed_value_with_same_timestamp_is_still_seen() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "map", Some(b"a"), 50).unwrap();
    t.commit();
    drop(w);
    let mut r = InfoReader::attach(&mem).unwrap();
    let mut w = InfoWriter::attach(&mut mem).unwrap();
    let mut t = w.begin();
    t.set("game_info", "map", Some(b"b"), 50).unwrap();
    assert!(t.commit());
    drop(w);
    let ch = r.changed(&mem);
    assert_eq!(ch.len(), 1);
    assert_eq!(ch[0].value.as_deref(), Some(&b"b"[..]));
}

#[test]
fn null_is_distinct_from_empty() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b""), 1).unwrap();
    t.set("game_info", "map", Some(b"x"), 1).unwrap();
    assert!(t.commit());
    let mut t = w.begin();
    t.set("game_info", "map", None, 2).unwrap();
    assert!(t.commit());
    drop(w);

    let snap = InfoReader::attach(&mem).unwrap().snapshot(&mem);
    let get = |k: &str| snap.iter().find(|c| c.key == k).unwrap().value.clone();
    assert_eq!(get("phase"), Some(vec![]));
    assert_eq!(get("map"), None);
}

#[test]
fn too_long_value_is_rejected() {
    let s = Schema { categories: vec![("c".into(), vec![("k".into(), 64)])] };
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("c", "k", Some(&[b'a'; 64]), 1).unwrap();
    t.commit();
    let mut t = w.begin();
    assert_eq!(t.set("c", "k", Some(&[b'b'; 65]), 2), Err(ChannelError::TooLong));
    assert!(!t.commit());
    drop(w);
    let snap = InfoReader::attach(&mem).unwrap().snapshot(&mem);
    assert_eq!(snap[0].value.as_deref(), Some(&[b'a'; 64][..]));
}

#[test]
fn unknown_key_is_rejected() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    assert_eq!(t.set("game_info", "nope", Some(b"x"), 1), Err(ChannelError::UnknownKey));
    assert_eq!(t.set("nope", "phase", Some(b"x"), 1), Err(ChannelError::UnknownKey));
}

#[test]
fn consumer_attaching_late_reads_full_snapshot() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"GameInProgress"), 10).unwrap();
    t.set("game_info", "map", Some(b"dl_midtown"), 11).unwrap();
    t.set("match_info", "roster_0", Some(b"{\"name\":\"a\"}"), 12).unwrap();
    assert!(t.commit());
    drop(w);

    let mut r = InfoReader::attach(&mem).unwrap();
    let snap = r.snapshot(&mem);
    assert_eq!(snap.len(), 3);
    assert!(snap.iter().any(|c| c.category == "match_info" && c.key == "roster_0"));
    assert!(r.changed(&mem).is_empty());
}

#[test]
fn reader_sees_changes_after_attach() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"a"), 1).unwrap();
    t.commit();
    drop(w);
    let mut r = InfoReader::attach(&mem).unwrap();
    let mut w = InfoWriter::attach(&mut mem).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"b"), 5).unwrap();
    t.set("game_info", "map", Some(b"m"), 5).unwrap();
    assert!(t.commit());
    drop(w);
    let ch = r.changed(&mem);
    assert_eq!(ch.len(), 2);
    assert!(r.changed(&mem).is_empty());
}

#[test]
fn writer_attach_keeps_existing_values() {
    let s = schema();
    let mut mem = region(&s);
    let mut w = InfoWriter::init(&mut mem, &s).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"keep"), 1).unwrap();
    t.commit();
    drop(w);
    let mut w = InfoWriter::attach(&mut mem).unwrap();
    let mut t = w.begin();
    t.set("game_info", "phase", Some(b"keep"), 9).unwrap();
    assert!(!t.commit());
    let mut t = w.begin();
    t.set("game_info", "map", Some(b"x"), 9).unwrap();
    assert!(t.commit());
    drop(w);
    assert_eq!(InfoReader::attach(&mem).unwrap().snapshot(&mem).len(), 2);
}

#[test]
fn bad_regions_are_rejected() {
    let s = schema();
    let mut mem = region(&s);
    assert!(matches!(InfoWriter::init(&mut mem[..10], &s), Err(ChannelError::Truncated)));
    assert!(matches!(InfoReader::attach(&[0u8; 64]), Err(ChannelError::BadMagic)));
    InfoWriter::init(&mut mem, &s).unwrap();
    let mut bad = mem.clone();
    bad[4] = 99;
    assert!(matches!(InfoReader::attach(&bad), Err(ChannelError::BadVersion)));
    assert!(matches!(InfoReader::attach(&mem[..mem.len() - 1]), Err(ChannelError::Truncated)));
    assert!(matches!(InfoWriter::attach(&mut [0u8; 64]), Err(ChannelError::BadMagic)));
}
