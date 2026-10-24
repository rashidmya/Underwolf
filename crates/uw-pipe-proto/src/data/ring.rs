//! The event ring (design 2b "Event ring"): a 24-byte header, then `RING_SLOTS` records.
//!
//! - Header: magic `UWDE` @0, format version u32 @4, `write_seq` u64 @8 (the last seq written,
//!   0 for none), `dropped` u64 @16 (events the producer's queue dropped).
//! - Record: `seq` u64 @0, `ts` u64 @8, `name_len` u16 @16, the name (up to 255 bytes) @18,
//!   `data_len` u32 @276, the data (up to 63 KiB) @280.
//!
//! Seqs start at 1, and record `seq` lives in slot `seq % RING_SLOTS`. The producer writes a
//! record and then `write_seq`, under the events lock. The helper reads with `RingReader`, under
//! the same lock, and checks the header and every record it reads.

use super::{
    FORMAT_VERSION, RING_SLOTS, ReadError, ResumePoint, catch_up, rd_u16, rd_u32, rd_u64,
    start_after, wr_u16, wr_u32, wr_u64,
};

pub const RING_MAGIC: [u8; 4] = *b"UWDE";
pub const RING_HEADER_SIZE: usize = 24;
pub const MAX_EVENT_NAME: usize = 255;
pub const MAX_EVENT_DATA: usize = 63 * 1024;
/// One record's size: 280 bytes of fields and name, then the data.
pub const RECORD_SIZE: usize = REC_DATA + MAX_EVENT_DATA;
/// The ring's size in bytes (about 1.85 MiB).
pub const RING_SIZE: usize = RING_HEADER_SIZE + RING_SLOTS as usize * RECORD_SIZE;

const OFF_VERSION: usize = 4;
const OFF_WRITE_SEQ: usize = 8;
const OFF_DROPPED: usize = 16;
const REC_SEQ: usize = 0;
const REC_TS: usize = 8;
const REC_NAME_LEN: usize = 16;
const REC_NAME: usize = 18;
const REC_DATA_LEN: usize = 276;
const REC_DATA: usize = 280;

fn slot(seq: u64) -> usize {
    RING_HEADER_SIZE + (seq % RING_SLOTS) as usize * RECORD_SIZE
}

/// The producer's side, at create: the header, no records. Panics if `region` is smaller than
/// `RING_SIZE`.
pub fn ring_init(region: &mut [u8]) {
    let region = &mut region[..RING_SIZE];
    region.fill(0);
    region[..4].copy_from_slice(&RING_MAGIC);
    wr_u32(region, OFF_VERSION, FORMAT_VERSION);
}

/// The producer's side, under the events lock: write the next record and then `write_seq`.
/// Returns the record's seq. Panics if the name or the data is over its limit
/// (`Outbox::push_event` checks that first).
pub fn ring_push(region: &mut [u8], name: &str, data: &str, ts: u64) -> u64 {
    assert!(
        name.len() <= MAX_EVENT_NAME && data.len() <= MAX_EVENT_DATA,
        "ring_push: a name or data over its limit"
    );
    let seq = rd_u64(region, OFF_WRITE_SEQ) + 1;
    let rec = &mut region[slot(seq)..slot(seq) + RECORD_SIZE];
    wr_u64(rec, REC_SEQ, seq);
    wr_u64(rec, REC_TS, ts);
    wr_u16(rec, REC_NAME_LEN, name.len() as u16);
    rec[REC_NAME..REC_NAME + name.len()].copy_from_slice(name.as_bytes());
    wr_u32(rec, REC_DATA_LEN, data.len() as u32);
    rec[REC_DATA..REC_DATA + data.len()].copy_from_slice(data.as_bytes());
    wr_u64(region, OFF_WRITE_SEQ, seq);
    seq
}

/// The producer's side, under the events lock: the queue's dropped count so far.
pub fn ring_set_dropped(region: &mut [u8], dropped: u64) {
    wr_u64(region, OFF_DROPPED, dropped);
}

/// The ring's header, checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingHead {
    pub write_seq: u64,
    pub dropped: u64,
}

/// Check the ring's size, magic and version, and read its counters.
pub fn read_ring_head(region: &[u8]) -> Result<RingHead, ReadError> {
    if region.len() < RING_SIZE {
        return Err(ReadError::Layout(format!(
            "event ring is {} bytes, under {RING_SIZE}",
            region.len()
        )));
    }
    if region[..4] != RING_MAGIC {
        return Err(ReadError::Layout("event ring magic is not UWDE".into()));
    }
    let version = rd_u32(region, OFF_VERSION);
    if version != FORMAT_VERSION {
        return Err(ReadError::Version(version));
    }
    Ok(RingHead {
        write_seq: rd_u64(region, OFF_WRITE_SEQ),
        dropped: rd_u64(region, OFF_DROPPED),
    })
}

/// One event as the helper read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingEvent {
    pub seq: u64,
    pub name: String,
    pub data: String,
    pub ts: u64,
}

/// One drain: `lost` events (`data_lost`, sent first), then `events` in seq order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Drained {
    pub lost: u64,
    pub events: Vec<RingEvent>,
}

fn read_record(region: &[u8], seq: u64) -> Result<RingEvent, ReadError> {
    let rec = &region[slot(seq)..slot(seq) + RECORD_SIZE];
    let layout_err = |why: String| Err(ReadError::Layout(why));
    let stored = rd_u64(rec, REC_SEQ);
    if stored != seq {
        return layout_err(format!("the ring slot for seq {seq} holds seq {stored}"));
    }
    let name_len = rd_u16(rec, REC_NAME_LEN) as usize;
    if name_len > MAX_EVENT_NAME {
        return layout_err(format!(
            "event {seq}: name length {name_len} over {MAX_EVENT_NAME}"
        ));
    }
    let data_len = rd_u32(rec, REC_DATA_LEN) as usize;
    if data_len > MAX_EVENT_DATA {
        return layout_err(format!(
            "event {seq}: data length {data_len} over {MAX_EVENT_DATA}"
        ));
    }
    let Ok(name) = String::from_utf8(rec[REC_NAME..REC_NAME + name_len].to_vec()) else {
        return layout_err(format!("event {seq}: name is not UTF-8"));
    };
    let Ok(data) = String::from_utf8(rec[REC_DATA..REC_DATA + data_len].to_vec()) else {
        return layout_err(format!("event {seq}: data is not UTF-8"));
    };
    Ok(RingEvent {
        seq,
        name,
        data,
        ts: rd_u64(rec, REC_TS),
    })
}

/// The helper's side: where it is in one producer's ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingReader {
    /// The last seq delivered (or the start point).
    after: u64,
    /// The producer's dropped count already reported (or seen at the start).
    dropped: u64,
}

impl RingReader {
    /// Attach to a ring (design flow step 4): check it and pick the start with `start_after`.
    /// Returns the reader and the start (`data_connected.from_seq`). Only `dropped` growth from
    /// here on is lost. A resume point ahead of the ring's write head is a bad layout.
    pub fn start(
        region: &[u8],
        producer_id: u64,
        resume: Option<ResumePoint>,
    ) -> Result<(RingReader, u64), ReadError> {
        let head = read_ring_head(region)?;
        let after = start_after(producer_id, head.write_seq, resume);
        catch_up(after, head.write_seq)?;
        Ok((
            RingReader {
                after,
                dropped: head.dropped,
            },
            after,
        ))
    }

    /// The last seq delivered.
    pub fn after(&self) -> u64 {
        self.after
    }

    /// Everything after the last drain, in seq order, and what was lost before it: records
    /// overwritten before they were read, plus growth in the producer's `dropped`. A seq or a
    /// dropped count that goes backwards is a bad layout.
    pub fn drain(&mut self, region: &[u8]) -> Result<Drained, ReadError> {
        let head = read_ring_head(region)?;
        if head.dropped < self.dropped {
            return Err(ReadError::Layout(format!(
                "the dropped count went backwards: {} after {}",
                head.dropped, self.dropped
            )));
        }
        let c = catch_up(self.after, head.write_seq)?;
        let mut events = Vec::new();
        for seq in c.first..=head.write_seq {
            events.push(read_record(region, seq)?);
        }
        let lost = c.lost + (head.dropped - self.dropped);
        self.after = head.write_seq;
        self.dropped = head.dropped;
        Ok(Drained { lost, events })
    }
}
