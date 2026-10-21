//! Overwolf-style data channel between the in-game collector (producer) and a Wine-side helper
//! (consumer): a keyed info store and an event ring over plain byte slices, plus a send throttle.
//! The portable modules are unit-tested on Linux; `win` holds the named shared-memory objects.

mod events;
mod info;
mod schema;
mod throttle;
#[cfg(windows)]
pub mod win;

pub use events::{
    DATA_MAX, EventRecord, NAME_MAX, RING_SLOTS, RingReader, ring_init, ring_push, ring_region_size,
};
pub use info::{InfoChange, InfoReader, InfoWriter, Txn, info_region_size};
pub use schema::Schema;
pub use throttle::Throttle;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    TooLong,
    UnknownKey,
    BadMagic,
    BadVersion,
    Truncated,
    Malformed,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ChannelError::TooLong => "value too long",
            ChannelError::UnknownKey => "unknown category or key",
            ChannelError::BadMagic => "bad region magic",
            ChannelError::BadVersion => "unsupported format version",
            ChannelError::Truncated => "region or schema truncated",
            ChannelError::Malformed => "malformed region or schema",
        })
    }
}

impl std::error::Error for ChannelError {}

pub(crate) fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

pub(crate) fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub(crate) fn wr_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
