//! Underwolf injection protocols. `pipe` is the fixed-layout binary channel between the Wine-side
//! helper and the in-game bootstrap (spec 6.3); `jsonl` is the temporary one-object-per-line
//! channel between the Linux host and the helper (plan 2 replaces it with a binary relay);
//! `names` is the per-game pipe and mutex names both sides must agree on. `bells` (doorbell bits
//! and the mask region) and `state` (the state block, layout v1) are the control channel's shared
//! memory layouts.

pub mod bells;
pub mod jsonl;
pub mod names;
pub mod pipe;
pub mod state;
