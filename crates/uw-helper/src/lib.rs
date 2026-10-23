//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine (unit-tested on Linux with a fake). The real `Win32`
//! implementation and the pipe wiring live in the `cfg(windows)` bin `uw-helper64`.

pub mod decide;
