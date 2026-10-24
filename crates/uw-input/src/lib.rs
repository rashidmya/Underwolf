//! Production input policy. Windows adapters execute effects outside policy locks.
pub mod policy;
pub mod text;

pub mod delivery;
pub mod hooks;
pub mod raw;
pub mod registration;
pub mod requests;
#[cfg(windows)]
pub mod win;

pub mod cursor;
