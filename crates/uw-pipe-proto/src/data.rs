//! The game-data channel's layouts (design 2b "Messages and layouts"), as byte-slice code with
//! fixed little-endian offsets, so they are unit-tested on Linux like `state`. The producer in the
//! game creates the objects and writes them; the helper only reads them, and checks every read.
//! The Win32 objects that hold these bytes are `uw_game_channel::data`.
//!
//! - `hello`: the 24-byte hello block (magic, format version, producer pid, `producer_id`).
//! - `schema`: the info store's categories of keys, each key with a maximum value length.
//! - `info`: the info store (header, schema, per-key timestamps and value slots).
//! - `ring`: the event ring (header, 30 records).
//! - `resume`: where event delivery starts and resumes, as pure arithmetic on seqs.
//! - `outbox`: the producer's pending info and event queue, with its throttle.

mod hello;
mod info;
mod outbox;
mod resume;
mod ring;
mod schema;

pub use hello::{HELLO_MAGIC, HELLO_SIZE, Hello, read_hello, write_hello};
pub use info::{
    INFO_HEADER_SIZE, INFO_MAGIC, InfoLayout, InfoReader, InfoValue, info_init, info_set,
};
pub use outbox::{Outbox, ProduceError, QUEUE_CAP, THROTTLE_MAX, THROTTLE_WINDOW_MS, Throttle};
pub use resume::{BadResumePoint, CatchUp, RING_SLOTS, ResumePoint, catch_up, start_after};
pub use ring::{
    Drained, MAX_EVENT_DATA, MAX_EVENT_NAME, RECORD_SIZE, RING_HEADER_SIZE, RING_MAGIC, RING_SIZE,
    RingEvent, RingHead, RingReader, read_ring_head, ring_init, ring_push, ring_set_dropped,
};
pub use schema::{
    MAX_CATEGORIES, MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, Schema, SchemaCategory, SchemaError,
    SchemaKey,
};

/// The format version of the hello block, the info store and the event ring.
pub const FORMAT_VERSION: u32 = 1;
/// The channel milestone 1 uses (design 2b "Scope"); the helper's `--data-channel` defaults to
/// it.
pub const DEFAULT_CHANNEL: u32 = 1;

/// Why bytes the helper read from the channel cannot be used (design 2b "Failures"). The helper
/// reports it once per producer, stops reading and waits for `disconnect`; it is not a stop code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A hello block or a header with another format version (`data_version_mismatch`).
    Version(u32),
    /// Anything else that does not fit (`data_bad_layout`), and why.
    Layout(String),
}

impl ReadError {
    /// The JSON `error` code the helper reports.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::Version(_) => "data_version_mismatch",
            ReadError::Layout(_) => "data_bad_layout",
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Version(v) => write!(f, "format version {v}, not {FORMAT_VERSION}"),
            ReadError::Layout(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for ReadError {}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}
fn wr_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
