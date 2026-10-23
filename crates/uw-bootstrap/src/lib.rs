//! The pinned in-game bootstrap DLL (`uw_bootstrap64.dll`). The helper loads it with a
//! `SetWindowsHookExW` thread hook; Wine then loads it into the game and calls `uw_bootstrap_hook`
//! there. On the first call with `code >= 0`, it pins itself (so unhooking cannot unload it) and,
//! only if the pin held, starts one worker thread; every call ends in `CallNextHookEx`. `DllMain`
//! does nothing, because this DLL is also loaded into the helper (where `SetWindowsHookExW`
//! requires a DLL-resident procedure).
//!
//! The worker (in `win`) holds the marker mutex, connects to the helper's pipe, sends `hello` and
//! serves `load_overlay` requests, reconnecting with a fresh `hello` when a connection is lost.
//! `state` is the portable overlay-load decision, unit-tested on Linux.

// Every FFI entry point relies on `catch_unwind`, which does nothing under `panic = "abort"`: a
// panic would then kill the game (design: no panic crosses FFI).
#[cfg(panic = "abort")]
compile_error!("uw-bootstrap needs panic = \"unwind\"");

pub mod state;

#[cfg(windows)]
mod win;
