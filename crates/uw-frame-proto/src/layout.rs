use crate::{
    AllocationKey, FORMAT_BGRA8, FrameError, HEADER_BYTES, MAX_SIDE, Result, SessionId, WidgetKey,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_len: u64,
    pub alignment: u64,
}
impl FrameLayout {
    pub fn new(width: u32, height: u32, alignment: u64) -> Result<Self> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(FrameError::Bounds);
        }
        if !alignment.is_power_of_two() || !(4096..=1_048_576).contains(&alignment) {
            return Err(FrameError::Unsupported);
        }
        let stride = width
            .checked_mul(4)
            .and_then(|n| n.checked_add(63))
            .ok_or(FrameError::Overflow)?
            & !63;
        let size = u64::from(stride) * u64::from(height);
        let pixel_len = size
            .checked_add(alignment - 1)
            .ok_or(FrameError::Overflow)?
            & !(alignment - 1);
        Ok(Self {
            width,
            height,
            stride,
            pixel_len,
            alignment,
        })
    }
    pub fn validate(self) -> Result<()> {
        if Self::new(self.width, self.height, self.alignment)? == self {
            Ok(())
        } else {
            Err(FrameError::Invalid)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationHeader {
    pub key: AllocationKey,
    pub consumer_pid: u32,
    pub producer_pid: u32,
    pub start_ticks: u64,
    pub layout: FrameLayout,
}
impl AllocationHeader {
    pub fn validate(self) -> Result<()> {
        self.key.validate()?;
        self.layout.validate()?;
        if self.consumer_pid == 0 || self.producer_pid == 0 || self.start_ticks == 0 {
            return Err(FrameError::Invalid);
        }
        Ok(())
    }
    pub fn encode(self) -> Result<[u8; 128]> {
        self.validate()?;
        let mut b = [0u8; 128];
        b[..4].copy_from_slice(b"UWFC");
        b[4..6].copy_from_slice(&1u16.to_le_bytes());
        b[6..8].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        b[8..24].copy_from_slice(&self.key.session.0);
        b[24..28].copy_from_slice(&self.consumer_pid.to_le_bytes());
        b[28..32].copy_from_slice(&self.producer_pid.to_le_bytes());
        b[32..40].copy_from_slice(&self.start_ticks.to_le_bytes());
        b[40..44].copy_from_slice(&self.key.widget.id.to_le_bytes());
        b[44..48].copy_from_slice(&self.key.widget.generation.to_le_bytes());
        b[48..56].copy_from_slice(&self.key.generation.to_le_bytes());
        b[56..60].copy_from_slice(&self.layout.width.to_le_bytes());
        b[60..64].copy_from_slice(&self.layout.height.to_le_bytes());
        b[64..68].copy_from_slice(&self.layout.stride.to_le_bytes());
        b[68..72].copy_from_slice(&FORMAT_BGRA8.to_le_bytes());
        b[72..80].copy_from_slice(&self.layout.pixel_len.to_le_bytes());
        b[80..88].copy_from_slice(&self.layout.alignment.to_le_bytes());
        b[88..90].copy_from_slice(&40u16.to_le_bytes());
        b[90..92].copy_from_slice(&8u16.to_le_bytes());
        b[92..96].copy_from_slice(&1u32.to_le_bytes());
        Ok(b)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 128 || &bytes[..4] != b"UWFC" {
            return Err(FrameError::Invalid);
        }
        if take::<2>(bytes, 4)? != 1u16.to_le_bytes()
            || take::<2>(bytes, 6)? != (HEADER_BYTES as u16).to_le_bytes()
        {
            return Err(FrameError::Version);
        }
        if take::<4>(bytes, 68)? != FORMAT_BGRA8.to_le_bytes()
            || take::<2>(bytes, 88)? != 40u16.to_le_bytes()
            || take::<2>(bytes, 90)? != 8u16.to_le_bytes()
            || take::<4>(bytes, 92)? != 1u32.to_le_bytes()
        {
            return Err(FrameError::Unsupported);
        }
        if bytes[96..].iter().any(|&b| b != 0) {
            return Err(FrameError::Invalid);
        }
        let h = Self {
            key: AllocationKey {
                session: SessionId(take(bytes, 8)?),
                widget: WidgetKey {
                    id: u32::from_le_bytes(take(bytes, 40)?),
                    generation: u32::from_le_bytes(take(bytes, 44)?),
                },
                generation: u64::from_le_bytes(take(bytes, 48)?),
            },
            consumer_pid: u32::from_le_bytes(take(bytes, 24)?),
            producer_pid: u32::from_le_bytes(take(bytes, 28)?),
            start_ticks: u64::from_le_bytes(take(bytes, 32)?),
            layout: FrameLayout {
                width: u32::from_le_bytes(take(bytes, 56)?),
                height: u32::from_le_bytes(take(bytes, 60)?),
                stride: u32::from_le_bytes(take(bytes, 64)?),
                pixel_len: u64::from_le_bytes(take(bytes, 72)?),
                alignment: u64::from_le_bytes(take(bytes, 80)?),
            },
        };
        h.validate()?;
        Ok(h)
    }
}

pub(crate) fn take<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N]> {
    bytes
        .get(at..at.checked_add(N).ok_or(FrameError::Overflow)?)
        .ok_or(FrameError::Invalid)?
        .try_into()
        .map_err(|_| FrameError::Invalid)
}
