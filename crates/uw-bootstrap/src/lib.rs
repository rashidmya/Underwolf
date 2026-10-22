//! The pinned in-game bootstrap DLL (`uw_bootstrap64.dll`). The helper loads it with a
//! `SetWindowsHookExW` thread hook; Wine then loads it into the game and calls `uw_bootstrap_hook`
//! there. On the first call only, it pins itself (so unhooking cannot unload it) and starts one
//! worker thread; every call ends in `CallNextHookEx`. `DllMain` does nothing, because this DLL is
//! also loaded into the helper (where `SetWindowsHookExW` requires a DLL-resident procedure).
//!
//! In this task the worker only proves the mechanism: it writes `%TEMP%\uw-bootstrap-proof-<pid>.txt`
//! with the game's pid. Task 5 adds `pub mod state` (portable) and grows `win` into the pipe
//! connect + overlay load.

#[cfg(windows)]
mod win;
