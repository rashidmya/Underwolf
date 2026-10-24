//! Checked x64 RAWINPUT walks. Only original-call output is passed here by the adapter.
use std::ops::Range;
pub const RELEASE_TAG: u32 = 0x55574952;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawError;
pub struct RawRecordIter<'a> {
    bytes: &'a [u8],
    left: u32,
    at: usize,
    failed: bool,
}
impl<'a> RawRecordIter<'a> {
    pub fn new(bytes: &'a [u8], count: u32) -> Self {
        Self { bytes, left: count, at: 0, failed: false }
    }
}
fn size(bytes: &[u8]) -> Result<usize, RawError> {
    if bytes.len() < 24 {
        return Err(RawError);
    }
    let kind = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let n = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let min = match kind {
        0 => 48,
        1 => 40,
        2 => 32,
        _ => return Err(RawError),
    };
    if n < min || n > bytes.len() { Err(RawError) } else { Ok(n) }
}
impl Iterator for RawRecordIter<'_> {
    type Item = Result<Range<usize>, RawError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 || self.failed {
            return None;
        }
        let n = self.bytes.get(self.at..).ok_or(RawError).and_then(size);
        match n {
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
            Ok(n) => {
                let span = self.at..self.at + n;
                self.at = (self.at + n).div_ceil(8) * 8;
                self.left -= 1;
                Some(Ok(span))
            }
        }
    }
}
#[derive(Default)]
pub struct ReleaseSet {
    keys: [u64; 4],
    buttons: u8,
}
impl ReleaseSet {
    pub fn key_up(&mut self, vk: u8) {
        self.keys[vk as usize / 64] |= 1u64 << (vk % 64)
    }
    pub fn mouse_up(&mut self, button: u8) {
        if button < 5 {
            self.buttons |= 1 << button
        }
    }
    fn contains_key(&self, vk: u16) -> bool {
        vk < 256 && self.keys[vk as usize / 64] & (1u64 << (vk % 64)) != 0
    }
}
pub fn neutralize(record: &mut [u8], releases: &mut ReleaseSet) -> Result<(), RawError> {
    let n = size(record)?;
    let kind = u32::from_le_bytes(record[..4].try_into().unwrap());
    match kind {
        0 => {
            let tag = u32::from_le_bytes(record[44..48].try_into().unwrap());
            let flags = u16::from_le_bytes(record[28..30].try_into().unwrap());
            let allowed = [2u16, 0x20, 8, 0x80, 0x200]
                .iter()
                .enumerate()
                .fold(0, |bits, (i, flag)| if releases.buttons & (1 << i) != 0 { bits | flag } else { bits });
            if tag != RELEASE_TAG || flags == 0 || flags & !allowed != 0 {
                record[28..44].fill(0)
            } else {
                // A release never carries game movement or wheel data.
                record[30..44].fill(0);
                for (i, bit) in [2u16, 0x20, 8, 0x80, 0x200].iter().enumerate() {
                    if flags & bit != 0 {
                        releases.buttons &= !(1 << i);
                    }
                }
            }
        }
        1 => {
            let tag = u32::from_le_bytes(record[36..40].try_into().unwrap());
            let flags = u16::from_le_bytes(record[26..28].try_into().unwrap());
            let vk = u16::from_le_bytes(record[30..32].try_into().unwrap());
            if tag != RELEASE_TAG || flags & 1 == 0 || !releases.contains_key(vk) {
                record[24..n].fill(0);
                record[26..28].copy_from_slice(&1u16.to_le_bytes());
                record[30..32].copy_from_slice(&255u16.to_le_bytes());
                record[32..36].copy_from_slice(&0x101u32.to_le_bytes());
            } else {
                releases.keys[vk as usize / 64] &= !(1u64 << (vk % 64));
            }
        }
        _ => {}
    }
    Ok(())
}
