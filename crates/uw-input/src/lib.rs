//! Production input policy. Windows adapters execute effects outside policy locks.
pub mod policy;
pub mod text;

pub mod delivery;
pub mod directinput;
pub mod hooks;
pub mod native;
pub mod raw;
pub mod registration;
pub mod requests;
#[cfg(windows)]
pub mod win;

pub mod configuration;
pub mod cursor;
pub mod hotkeys;
pub mod routing;
