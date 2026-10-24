//! Where event delivery starts and resumes (design 2b "Failures"), as pure arithmetic on seqs.
//! The helper starts after the resume point `uw-host` gave it when that point names the same
//! producer, and at the write head otherwise. A start that the ring has already overwritten
//! loses the gap, and the rest is delivered once each.

use super::ReadError;

/// The ring's record count: record `seq` lives in slot `seq % RING_SLOTS`.
pub const RING_SLOTS: u64 = 30;

/// `--data-resume <producer_id>:<seq>` (design 2b "Helper arguments"): a producer, and the last
/// seq `uw-host` delivered from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePoint {
    pub producer_id: u64,
    pub seq: u64,
}

impl std::fmt::Display for ResumePoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.producer_id, self.seq)
    }
}

/// Why a `--data-resume` value is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadResumePoint(pub String);

impl std::fmt::Display for BadResumePoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not <producer_id>:<seq>", self.0)
    }
}
impl std::error::Error for BadResumePoint {}

impl std::str::FromStr for ResumePoint {
    type Err = BadResumePoint;

    fn from_str(s: &str) -> Result<ResumePoint, BadResumePoint> {
        let bad = || BadResumePoint(s.to_string());
        let (id, seq) = s.split_once(':').ok_or_else(bad)?;
        Ok(ResumePoint {
            producer_id: id.parse().map_err(|_| bad())?,
            seq: seq.parse().map_err(|_| bad())?,
        })
    }
}

/// The seq after which a newly attached helper delivers events (design flow step 4): the resume
/// point's seq if it names this producer, otherwise the write head (`write_seq`), as on a first
/// attach.
pub fn start_after(producer_id: u64, write_seq: u64, resume: Option<ResumePoint>) -> u64 {
    match resume {
        Some(r) if r.producer_id == producer_id => r.seq,
        _ => write_seq,
    }
}

/// What to deliver after `after`, the last seq delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchUp {
    /// Records overwritten before they could be read: `data_lost`.
    pub lost: u64,
    /// The first record to deliver; the rest follow up to `write_seq`.
    pub first: u64,
}

/// With the ring's last written seq `write_seq`, the records to deliver after `after`:
/// `first..=write_seq`, behind `lost` records the ring no longer holds. A `write_seq` behind
/// `after` is a seq that went backwards under the same producer: a bad layout.
pub fn catch_up(after: u64, write_seq: u64) -> Result<CatchUp, ReadError> {
    if after > write_seq {
        return Err(ReadError::Layout(format!(
            "event seq went backwards: the ring is at {write_seq}, after {after}"
        )));
    }
    let oldest = write_seq.saturating_sub(RING_SLOTS - 1).max(1);
    let next = after.saturating_add(1);
    let first = next.max(oldest);
    Ok(CatchUp {
        lost: first - next,
        first,
    })
}
