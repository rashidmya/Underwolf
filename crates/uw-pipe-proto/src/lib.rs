//! Underwolf injection and control-channel protocols. `pipe` is the fixed-layout binary framing
//! of the helper's two pipes (bootstrap and overlay, spec 6.3); `jsonl` is the one-object-per-line
//! channel between the Linux host and the helper; `names` is the per-game pipe, mutex, event and
//! mapping names every side must agree on. `bells` (doorbell bits and the mask region) and `state`
//! (the state block, layout v1) are the control channel's shared-memory layouts; `data` is the
//! game-data channel's (design 2b).

pub mod bells;
pub mod data;
pub mod jsonl;
pub mod names;
pub mod pipe;
pub mod state;
