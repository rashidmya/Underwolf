//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine, `pending` the relay's `load_overlay` requests
//! awaiting their `status`, and `data_pass` the data thread's reads on each pass (all unit-tested
//! on Linux). The real `Win32` implementation, the pipe servers, the relay loop and the data
//! thread live in the `cfg(windows)` bin `uw-helper64`.

pub mod data_pass;
pub mod decide;
pub mod pending;

pub mod input;

pub mod routing;
