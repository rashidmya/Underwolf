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
    /// The wire names, for `bit_names`.
    pub const NAMES: [(u64, &str); 4] =
        [(EDIT_MODE_ON, "edit_mode_on"), (EDIT_MODE_OFF, "edit_mode_off"), (SHOW, "show"), (HIDE, "hide")];
}

/// Game -> host bits.
pub mod to_host {
    pub const STATE_APPLIED: u64 = 1 << 0;
    /// The wire names, for `bit_names`.
    pub const NAMES: [(u64, &str); 1] = [(STATE_APPLIED, "state_applied")];
}

/// The names of the bits set in `bits`, lowest bit first. A bit missing from `known` is named
/// `bit<N>` (design: the JSON `bell` event names unknown bits that way).
pub fn bit_names(bits: u64, known: &[(u64, &str)]) -> Vec<String> {
    (0..64)
        .map(|n| 1u64 << n)
        .filter(|bit| bits & bit != 0)
        .map(|bit| match known.iter().find(|(b, _)| *b == bit) {
            Some((_, name)) => name.to_string(),
            None => format!("bit{}", bit.trailing_zeros()),
        })
        .collect()
}
