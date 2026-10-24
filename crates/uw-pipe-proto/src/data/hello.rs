//! The hello block (design 2b "Hello block"), 24 bytes, little-endian:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | magic `UWDH` |
//! | 4 | format version u32 (1) |
//! | 8 | producer pid u32 |
//! | 12 | reserved u32 |
//! | 16 | `producer_id` u64 |
//!
//! The producer writes it last, just before it sets `connect`. `producer_id` is the producer's
//! `GetTickCount64()` at creation: with the pid, it tells one game session from the next.

use super::{FORMAT_VERSION, ReadError, rd_u32, rd_u64, wr_u32, wr_u64};

pub const HELLO_SIZE: usize = 24;
pub const HELLO_MAGIC: [u8; 4] = *b"UWDH";

const OFF_VERSION: usize = 4;
const OFF_PID: usize = 8;
const OFF_RESERVED: usize = 12;
const OFF_PRODUCER_ID: usize = 16;

/// The fields of a hello block that checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub pid: u32,
    pub producer_id: u64,
}

/// The producer's side: write the whole block. Panics if `b` is shorter than `HELLO_SIZE`.
pub fn write_hello(b: &mut [u8], pid: u32, producer_id: u64) {
    b[..4].copy_from_slice(&HELLO_MAGIC);
    wr_u32(b, OFF_VERSION, FORMAT_VERSION);
    wr_u32(b, OFF_PID, pid);
    wr_u32(b, OFF_RESERVED, 0);
    wr_u64(b, OFF_PRODUCER_ID, producer_id);
}

/// The helper's check (design flow step 2): the magic, the format version, and that the
/// producer is the game this helper serves (`game_pid`).
pub fn read_hello(b: &[u8], game_pid: u32) -> Result<Hello, ReadError> {
    if b.len() < HELLO_SIZE {
        return Err(ReadError::Layout(format!(
            "hello block is {} bytes, under {HELLO_SIZE}",
            b.len()
        )));
    }
    if b[..4] != HELLO_MAGIC {
        return Err(ReadError::Layout("hello block magic is not UWDH".into()));
    }
    let version = rd_u32(b, OFF_VERSION);
    if version != FORMAT_VERSION {
        return Err(ReadError::Version(version));
    }
    let pid = rd_u32(b, OFF_PID);
    if pid != game_pid {
        return Err(ReadError::Layout(format!(
            "hello block pid {pid}, not the game's {game_pid}"
        )));
    }
    Ok(Hello {
        pid,
        producer_id: rd_u64(b, OFF_PRODUCER_ID),
    })
}
