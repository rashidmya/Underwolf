//! Portable, versioned widget frame contracts. No platform calls or shared pointers.
mod layout;
mod rect;
mod state;
mod wire;
pub use layout::*;
pub use rect::*;
pub use state::*;
pub use wire::*;

pub const CONTROL_BYTES: usize = 4096;
pub const HEADER_BYTES: usize = 256;
pub const STATE_OFFSET: usize = 192;
pub const STATE_BYTES: usize = 64;
pub const MAX_WIDGETS: u32 = 32;
pub const MAX_SIDE: u32 = 4096;
pub const SHARED_BUDGET: u64 = 268_435_456;
pub const MAX_PAYLOAD: usize = 4096;
pub const MAX_FDS: usize = 2;
pub const FORMAT_BGRA8: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Invalid,
    Bounds,
    Overflow,
    Version,
    Stale,
    Busy,
    Unsupported,
    Capacity,
    OwnerDied,
    Io,
}
impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "frame error: {self:?}")
    }
}
impl std::error::Error for FrameError {}
pub type Result<T> = std::result::Result<T, FrameError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub [u8; 16]);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetKey {
    pub id: u32,
    pub generation: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AllocationKey {
    pub session: SessionId,
    pub widget: WidgetKey,
    pub generation: u64,
}

impl AllocationKey {
    pub fn validate(self) -> Result<()> {
        self.widget.validate()?;
        if self.session.0 == [0; 16] || self.generation == 0 {
            Err(FrameError::Invalid)
        } else {
            Ok(())
        }
    }
}
impl WidgetKey {
    pub fn validate(self) -> Result<()> {
        if self.id == 0 || self.generation == 0 {
            Err(FrameError::Invalid)
        } else {
            Ok(())
        }
    }
}
