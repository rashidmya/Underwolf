//! The state block, layout v1 (design "State block layout v1"): `Local\underwolf-<pid>-state`,
//! fixed little-endian offsets, no pointers. The helper creates the block and writes its header
//! once; after that both sides read and write the fields only while holding
//! `Local\underwolf-<pid>-state-lock`. Everything here works on the mapped bytes as a plain slice,
//! so it is unit-tested on Linux.
//!
//! | Offset | Field | Type | Writer |
//! |---|---|---|---|
//! | 0 | magic `"UWS1"` | [u8; 4] | helper, at creation |
//! | 4 | layout version (1) | u16 | helper, at creation |
//! | 8 | size of the block in bytes | u32 | helper, at creation |
//! | 16 | `seq` | u64 | helper |
//! | 24 | `applied_seq` | u64 | overlay |
//! | 32 | `edit_mode` (0/1) | u8 | helper |
//! | 33 | `visible` (0/1) | u8 | helper |

use crate::bells::to_game;

pub const MAGIC: [u8; 4] = *b"UWS1";
pub const LAYOUT_VERSION: u16 = 1;
/// Bytes in a layout v1 block: the header's `size` field, and the size the helper maps.
pub const BLOCK_SIZE: usize = 64;

const OFF_VERSION: usize = 4;
const OFF_SIZE: usize = 8;
const OFF_SEQ: usize = 16;
const OFF_APPLIED_SEQ: usize = 24;
const OFF_EDIT_MODE: usize = 32;
const OFF_VISIBLE: usize = 33;

/// The fields of a layout v1 block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateV1 {
    pub seq: u64,
    pub applied_seq: u64,
    pub edit_mode: bool,
    pub visible: bool,
}

/// Why a block's header is not layout v1. The helper refuses such a block with
/// `version_mismatch` ("restart the game") and never rewrites it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// The mapped bytes are fewer than `BLOCK_SIZE` (carries the length seen).
    Short(usize),
    BadMagic,
    BadVersion(u16),
    BadSize(u32),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderError::Short(n) => write!(f, "state block is {n} bytes, under {BLOCK_SIZE}"),
            HeaderError::BadMagic => write!(f, "state block magic is not UWS1"),
            HeaderError::BadVersion(v) => write!(f, "state block layout {v}, not {LAYOUT_VERSION}"),
            HeaderError::BadSize(s) => write!(f, "state block size field {s}, not {BLOCK_SIZE}"),
        }
    }
}
impl std::error::Error for HeaderError {}

/// The outcome of a `set_state`: `seq` after it, and the `to_game` bits of the fields that
/// changed. `bits == 0` means nothing changed and `seq` was not bumped (so no bell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub bits: u64,
}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Write the v1 header into a block the helper just created (all zero). Panics if `b` is shorter
/// than `BLOCK_SIZE`.
pub fn init_header(b: &mut [u8]) {
    b[..4].copy_from_slice(&MAGIC);
    b[OFF_VERSION..OFF_VERSION + 2].copy_from_slice(&LAYOUT_VERSION.to_le_bytes());
    b[OFF_SIZE..OFF_SIZE + 4].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
}

/// Check magic, layout version and size (design: a mismatch is `version_mismatch`).
pub fn check_header(b: &[u8]) -> Result<(), HeaderError> {
    if b.len() < BLOCK_SIZE {
        return Err(HeaderError::Short(b.len()));
    }
    if b[..4] != MAGIC {
        return Err(HeaderError::BadMagic);
    }
    let version = rd_u16(b, OFF_VERSION);
    if version != LAYOUT_VERSION {
        return Err(HeaderError::BadVersion(version));
    }
    let size = rd_u32(b, OFF_SIZE);
    if size as usize != BLOCK_SIZE {
        return Err(HeaderError::BadSize(size));
    }
    Ok(())
}

/// Read the fields. Panics if `b` is shorter than `BLOCK_SIZE`.
pub fn read(b: &[u8]) -> StateV1 {
    StateV1 {
        seq: rd_u64(b, OFF_SEQ),
        applied_seq: rd_u64(b, OFF_APPLIED_SEQ),
        edit_mode: b[OFF_EDIT_MODE] != 0,
        visible: b[OFF_VISIBLE] != 0,
    }
}

/// The overlay records the last `seq` it applied.
pub fn set_applied_seq(b: &mut [u8], seq: u64) {
    b[OFF_APPLIED_SEQ..OFF_APPLIED_SEQ + 8].copy_from_slice(&seq.to_le_bytes());
}

/// The helper's `set_state`: write the given fields that differ, and bump `seq` once if any did.
pub fn apply(b: &mut [u8], edit_mode: Option<bool>, visible: Option<bool>) -> Change {
    let mut bits = 0;
    if let Some(on) = edit_mode
        && (b[OFF_EDIT_MODE] != 0) != on
    {
        b[OFF_EDIT_MODE] = on as u8;
        bits |= if on { to_game::EDIT_MODE_ON } else { to_game::EDIT_MODE_OFF };
    }
    if let Some(on) = visible
        && (b[OFF_VISIBLE] != 0) != on
    {
        b[OFF_VISIBLE] = on as u8;
        bits |= if on { to_game::SHOW } else { to_game::HIDE };
    }
    let mut seq = rd_u64(b, OFF_SEQ);
    if bits != 0 {
        seq = seq.wrapping_add(1);
        b[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&seq.to_le_bytes());
    }
    Change { seq, bits }
}
