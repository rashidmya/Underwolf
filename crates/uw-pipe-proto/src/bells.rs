//! Doorbell bits v1 and the mask region (design "Messages and layouts"). Two auto-reset events
//! ring the other side; the 16-byte region `Local\underwolf-<pid>-bells` says why: `to_game` (u64
//! at offset 0) and `to_host` (u64 at offset 8). Senders OR bits in with an atomic operation, then
//! signal; receivers swap the mask to 0. A receiver ignores bits it does not know, so later plans
//! add bits without a version change.

/// Size of the mask region in bytes.
pub const MASK_REGION_SIZE: usize = 16;
/// Offset of the host -> game mask.
pub const TO_GAME_OFFSET: usize = 0;
/// Offset of the game -> host mask.
pub const TO_HOST_OFFSET: usize = 8;

/// Host -> game bits.
pub mod to_game {
    pub const EDIT_MODE_ON: u64 = 1 << 0;
    pub const EDIT_MODE_OFF: u64 = 1 << 1;
    pub const SHOW: u64 = 1 << 2;
    pub const HIDE: u64 = 1 << 3;
}

/// Game -> host bits.
pub mod to_host {
    pub const STATE_APPLIED: u64 = 1 << 0;
}
