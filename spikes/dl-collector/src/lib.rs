//! Deadlock collector core: bounded reads of the game's own memory, byte-pattern roots and
//! schema field offsets resolved by class and field name. Everything goes through `MemRead`, so
//! the same code runs in-game and offline against a `client.dll` file image.

pub mod collector;
pub mod keys;
pub mod mem;
pub mod module;
pub mod pattern;
#[cfg(windows)]
mod plugin;
pub mod roots;
pub mod runner;
pub mod schema;
pub mod tasks;
pub mod timing;
