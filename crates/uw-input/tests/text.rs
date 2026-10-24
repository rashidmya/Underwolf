use uw_input::text::Utf16Text;
#[test]
fn surrogate_pairs_and_malformed_sequences_preserve_following_text() {
    let mut t = Utf16Text::default();
    assert_eq!(t.push(0xd83d), [None, None]);
    assert_eq!(t.push(0xde00), [Some('😀'), None]);
    assert_eq!(t.push(0xdc00), [Some('\u{fffd}'), None]);
    t.push(0xd800);
    assert_eq!(t.push(65), [Some('\u{fffd}'), Some('A')]);
    t.push(0xd800);
    t.reset();
    assert_eq!(t.push(66), [Some('B'), None]);
}
