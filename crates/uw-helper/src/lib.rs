//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine, and `pending` the relay's `load_overlay` requests
//! awaiting their `status` (both unit-tested on Linux). The real `Win32` implementation, the pipe
//! servers and the relay loop live in the `cfg(windows)` bin `uw-helper64`.

pub mod decide;
pub mod pending;
