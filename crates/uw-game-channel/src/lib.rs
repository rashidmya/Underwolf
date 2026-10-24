//! The control channel between the helper and the in-game overlay (design 2a). `objects` holds
//! the per-game named objects: two auto-reset doorbell events, the 16-byte mask region, the state
//! block and its lock. The helper creates them before its pipes; the game only opens them, after
//! its `overlay_hello`. `start` runs the in-game worker an overlay module uses for all of it
//! (`worker`). `data` is the game-data channel (design 2b): the in-game `Producer` creates its
//! objects, and the helper's `Consumer` reads them. Windows only.

// The worker relies on `catch_unwind` to survive a panic in the overlay's callback; under
// `panic = "abort"` a panic would kill the game instead.
#[cfg(panic = "abort")]
compile_error!("uw-game-channel needs panic = \"unwind\"");

pub use uw_pipe_proto::bells::{to_game, to_host};
pub use uw_pipe_proto::state::StateV1;

#[cfg(windows)]
pub mod data;
#[cfg(windows)]
pub mod objects;
#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use worker::{start, Link};

pub mod input;
