//! Event ring: 30 fixed-size records, record `seq % 30` is overwritten.
//!
//! Header `{magic "UWGE", version u32, write_seq u64}`; record `{seq u64, ts u64, name_len u16,
//! name[255], pad, data_len u32, data[63 KiB]}`. Sequence numbers start at 1; `write_seq` is the
//! last one written (0 = none) and is stored after the record, so a record is complete once the
//! sequence has advanced past it.

use crate::{ChannelError, FORMAT_VERSION, rd_u16, rd_u32, rd_u64, wr_u16, wr_u32, wr_u64};

pub const RING_SLOTS: usize = 30;
pub const NAME_MAX: usize = 255;
pub const DATA_MAX: usize = 63 * 1024;

const MAGIC: &[u8; 4] = b"UWGE";
const HEADER: usize = 16;
const OFF_SEQ: usize = 8;
const REC_SEQ: usize = 0;
const REC_TS: usize = 8;
const REC_NAME_LEN: usize = 16;
const REC_NAME: usize = 18;
const REC_DATA_LEN: usize = 276;
const REC_DATA: usize = 280;
const RECORD: usize = REC_DATA + DATA_MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub name: String,
    pub data: Vec<u8>,
    pub ts: u64,
}

pub fn ring_region_size() -> usize {
    HEADER + RING_SLOTS * RECORD
}

fn check(region: &[u8]) -> Result<(), ChannelError> {
    if region.len() < ring_region_size() {
        return Err(ChannelError::Truncated);
    }
    if &region[..4] != MAGIC {
        return Err(ChannelError::BadMagic);
    }
    if rd_u32(region, 4) != FORMAT_VERSION {
        return Err(ChannelError::BadVersion);
    }
    Ok(())
}

fn slot(seq: u64) -> usize {
    HEADER + (seq % RING_SLOTS as u64) as usize * RECORD
}

/// Zeroes the region and writes the header. A region smaller than `ring_region_size()` is left
/// without a header, so later pushes fail with `Truncated`/`BadMagic`.
pub fn ring_init(region: &mut [u8]) {
    region.fill(0);
    if region.len() >= HEADER {
        region[..4].copy_from_slice(MAGIC);
        wr_u32(region, 4, FORMAT_VERSION);
    }
}

pub fn ring_push(region: &mut [u8], name: &str, data: &[u8], ts: u64) -> Result<(), ChannelError> {
    if name.len() > NAME_MAX || data.len() > DATA_MAX {
        return Err(ChannelError::TooLong);
    }
    check(region)?;
    // A stored write_seq of u64::MAX (corruption) restarts the sequence; readers resynchronise.
    let seq = rd_u64(region, OFF_SEQ).checked_add(1).unwrap_or(1);
    let rec = &mut region[slot(seq)..][..RECORD];
    wr_u64(rec, REC_SEQ, seq);
    wr_u64(rec, REC_TS, ts);
    wr_u16(rec, REC_NAME_LEN, name.len() as u16);
    rec[REC_NAME..REC_NAME + name.len()].copy_from_slice(name.as_bytes());
    wr_u32(rec, REC_DATA_LEN, data.len() as u32);
    rec[REC_DATA..REC_DATA + data.len()].copy_from_slice(data);
    wr_u64(region, OFF_SEQ, seq);
    Ok(())
}

pub struct RingReader {
    next_seq: u64,
}

impl RingReader {
    /// Starts before the oldest record, so the first `drain` returns everything still retained.
    pub fn new() -> RingReader {
        RingReader { next_seq: 1 }
    }

    /// Starts after the newest record: only events pushed from now on are returned.
    pub fn from_tail(region: &[u8]) -> RingReader {
        let seq = if check(region).is_ok() { rd_u64(region, OFF_SEQ) } else { 0 };
        RingReader { next_seq: seq.saturating_add(1) }
    }

    /// Returns the new records in order and the number lost (overwritten before they were read).
    pub fn drain(&mut self, region: &[u8]) -> (Vec<EventRecord>, u64) {
        if check(region).is_err() {
            return (Vec::new(), 0);
        }
        let write_seq = rd_u64(region, OFF_SEQ);
        if write_seq == u64::MAX {
            return (Vec::new(), 0);
        }
        let oldest = write_seq.saturating_sub(RING_SLOTS as u64 - 1).max(1);
        let mut lost = 0;
        if self.next_seq > write_seq + 1 {
            // The writer's sequence went backwards (region re-initialised): start over.
            self.next_seq = oldest;
        } else if self.next_seq < oldest {
            lost = oldest - self.next_seq;
            self.next_seq = oldest;
        }
        // From here at most RING_SLOTS records are read, and `next_seq + 1` cannot overflow.
        let mut out = Vec::new();
        while self.next_seq <= write_seq {
            let rec = &region[slot(self.next_seq)..][..RECORD];
            let name_len = rd_u16(rec, REC_NAME_LEN) as usize;
            let data_len = rd_u32(rec, REC_DATA_LEN) as usize;
            if rd_u64(rec, REC_SEQ) == self.next_seq && name_len <= NAME_MAX && data_len <= DATA_MAX {
                out.push(EventRecord {
                    name: String::from_utf8_lossy(&rec[REC_NAME..REC_NAME + name_len]).into_owned(),
                    data: rec[REC_DATA..REC_DATA + data_len].to_vec(),
                    ts: rd_u64(rec, REC_TS),
                });
            } else {
                lost += 1;
            }
            self.next_seq += 1;
        }
        (out, lost)
    }
}

impl Default for RingReader {
    fn default() -> Self {
        Self::new()
    }
}
