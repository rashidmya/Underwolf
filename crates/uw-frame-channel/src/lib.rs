//! Linux descriptor and shared-buffer ownership; the wire types stay portable.
pub use uw_frame_proto;
#[cfg(target_os = "linux")]
mod budget;
#[cfg(target_os = "linux")]
mod channel;
#[cfg(target_os = "linux")]
mod mapping;
#[cfg(target_os = "linux")]
mod memory;
#[cfg(target_os = "linux")]
mod mutex;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
pub use {budget::*, channel::*, mapping::*, session::*};
pub type Result<T> = std::result::Result<T, uw_frame_proto::FrameError>;
