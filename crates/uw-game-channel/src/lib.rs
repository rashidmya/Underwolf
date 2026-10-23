//! The control channel between the helper and the in-game overlay (design 2a). `objects` holds
//! the per-game named objects: two auto-reset doorbell events, the 16-byte mask region, the state
//! block and its lock. The helper creates them before its pipes; the game only opens them, after
//! its `overlay_hello`. Windows only.

#[cfg(windows)]
pub mod objects;
