//! Synchronous producer operations; no JavaScript value enters a socket worker.
mod values;
pub use values::*;
#[cfg(target_os = "linux")]
mod producer;
#[cfg(target_os = "linux")]
pub use producer::*;
#[cfg(all(target_os = "linux", feature = "node"))]
pub mod node;
