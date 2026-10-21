use uw_spike_gep_channel::Schema;

fn sample() -> Schema {
    Schema {
        categories: vec![
            ("gep_internal".into(), vec![("version".into(), 64), ("plugin_status".into(), 32)]),
            ("match_info".into(), vec![("roster_0".into(), 2048)]),
        ],
    }
}

#[test]
fn schema_roundtrip() {
    let s = sample();
    let parsed = Schema::parse(&s.serialize()).unwrap();
    assert_eq!(parsed, s);
    assert_eq!(parsed.key_index("match_info", "roster_0"), Some(2));
    assert_eq!(parsed.key_index("gep_internal", "version"), Some(0));
    assert_eq!(parsed.key_index("match_info", "nope"), None);
}

#[test]
fn truncated_schema_is_rejected() {
    let b = sample().serialize();
    for cut in 0..b.len() {
        assert!(Schema::parse(&b[..cut]).is_err(), "cut at {cut}");
    }
}
