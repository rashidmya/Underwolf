use uw_frame_proto::SessionId;
use uw_render_bridge::*;
#[test]
fn counters_remain_exact_and_noncanonical_values_are_rejected() {
    assert_eq!(
        parse_counter("9007199254740993", false).unwrap(),
        9007199254740993
    );
    assert_eq!(
        parse_counter("18446744073709551615", false).unwrap(),
        u64::MAX
    );
    assert_eq!(parse_counter("0", true).unwrap(), 0);
    for s in ["", "-1", "+1", "01", " 1", "1.0", "18446744073709551616"] {
        assert!(parse_counter(s, true).is_err());
    }
    assert!(parse_counter("0", false).is_err());
}
#[test]
fn opaque_session_keys_roundtrip_without_accepting_alias_spellings() {
    let id = SessionId([0x1a; 16]);
    assert_eq!(format_session(id), "1a".repeat(16));
    assert_eq!(parse_session(&"1a".repeat(16)).unwrap(), id);
    for s in [
        "",
        "1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A",
        "00000000000000000000000000000000",
        "gggggggggggggggggggggggggggggggg",
    ] {
        assert!(parse_session(s).is_err());
    }
}
#[test]
fn javascript_numbers_cannot_silently_wrap_or_truncate() {
    assert_eq!(uint(4294967295.0).unwrap(), u32::MAX);
    assert_eq!(int(-2147483648.0).unwrap(), i32::MIN);
    for n in [-1.0, 4294967296.0, 1.5, f64::NAN, f64::INFINITY] {
        assert!(uint(n).is_err());
    }
    for n in [-2147483649.0, 2147483648.0, 1.5, f64::NAN] {
        assert!(int(n).is_err());
    }
}
