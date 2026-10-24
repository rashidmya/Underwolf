//! A character-message decoder. A malformed pair can produce replacement plus a new character.
#[derive(Default)]
pub struct Utf16Text {
    high: Option<u16>,
}
impl Utf16Text {
    pub fn reset(&mut self) {
        self.high = None;
    }
    pub fn push(&mut self, unit: u16) -> [Option<char>; 2] {
        let high = self.high.take();
        if let Some(h) = high
            && (0xdc00..=0xdfff).contains(&unit)
        {
            return [char::from_u32(0x10000 + (((h as u32 - 0xd800) << 10) | (unit as u32 - 0xdc00))), None];
        }
        let prefix = high.map(|_| '\u{fffd}');
        let next = if (0xd800..=0xdbff).contains(&unit) {
            self.high = Some(unit);
            None
        } else {
            Some(char::from_u32(unit as u32).unwrap_or('\u{fffd}'))
        };
        if prefix.is_some() { [prefix, next] } else { [next, None] }
    }
}
