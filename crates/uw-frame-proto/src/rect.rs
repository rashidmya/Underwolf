use crate::{FrameError, Result};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub fn checked(
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        bounds: (u32, u32),
    ) -> Result<Option<Self>> {
        let right = x.checked_add(width).ok_or(FrameError::Overflow)?;
        let bottom = y.checked_add(height).ok_or(FrameError::Overflow)?;
        if right > bounds.0 || bottom > bounds.1 {
            return Err(FrameError::Bounds);
        }
        if width == 0 || height == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            x,
            y,
            width,
            height,
        }))
    }
    /// Both rectangles must already have been checked against the same bounds.
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self
            .x
            .checked_add(self.width)
            .expect("checked rectangle")
            .max(other.x.checked_add(other.width).expect("checked rectangle"));
        let bottom = self
            .y
            .checked_add(self.height)
            .expect("checked rectangle")
            .max(
                other
                    .y
                    .checked_add(other.height)
                    .expect("checked rectangle"),
            );
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}
