use crate::{
    AllocationHeader, AllocationKey, FrameError, FrameLayout, MAX_PAYLOAD, MAX_SIDE, MAX_WIDGETS,
    Result, SHARED_BUDGET, SessionId, WidgetKey,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Hello {
        pid: u32,
        start_ticks: u64,
        alignment: u64,
        mutex_size: u16,
        mutex_align: u16,
        abi: u32,
        capability: u32,
    },
    Welcome {
        session: SessionId,
        producer_pid: u32,
        max_widgets: u32,
        max_side: u32,
        shared_budget: u64,
    },
    Register(AllocationHeader),
    Dirty {
        key: AllocationKey,
        produced: u64,
    },
    Consumed {
        key: AllocationKey,
        consumed: u64,
    },
    Layout {
        widget: WidgetKey,
        revision: u64,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        z: i32,
        visible: bool,
    },
    Remove {
        widget: WidgetKey,
        revision: u64,
    },
    Dimming {
        revision: u64,
        active: bool,
    },
    NeedBaseline {
        widget: WidgetKey,
    },
    Unavailable {
        key: Option<AllocationKey>,
        reason: u32,
    },
    Retired {
        key: AllocationKey,
    },
    Registered {
        key: AllocationKey,
    },
    Swapchain {
        generation: u64,
        width: u32,
        height: u32,
        ready: bool,
    },
    Progress {
        swapchain_generation: u64,
        completed_draw: u64,
        monotonic_ns: u64,
    },
}
impl Message {
    pub fn expected_fds(&self) -> usize {
        if matches!(self, Self::Register(_)) {
            2
        } else {
            0
        }
    }
    pub fn validate(&self) -> Result<()> {
        match *self {
            Self::Hello {
                pid,
                start_ticks,
                alignment,
                mutex_size,
                mutex_align,
                abi,
                capability,
            } => {
                if pid == 0 || start_ticks == 0 || capability > 1 {
                    return Err(FrameError::Invalid);
                }
                if mutex_size != 40 || mutex_align != 8 || abi != 1 {
                    return Err(FrameError::Unsupported);
                }
                if capability == 1 {
                    FrameLayout::new(1, 1, alignment)?;
                }
            }
            Self::Welcome {
                session,
                producer_pid,
                max_widgets,
                max_side,
                shared_budget,
            } => {
                if session.0 == [0; 16] || producer_pid == 0 {
                    return Err(FrameError::Invalid);
                }
                if max_widgets != MAX_WIDGETS
                    || max_side != MAX_SIDE
                    || shared_budget != SHARED_BUDGET
                {
                    return Err(FrameError::Unsupported);
                }
            }
            Self::Register(header) => header.validate()?,
            Self::Dirty { key, produced: seq } | Self::Consumed { key, consumed: seq } => {
                key.validate()?;
                nonzero(seq)?;
            }
            Self::Layout {
                widget,
                revision,
                width,
                height,
                visible,
                ..
            } => {
                widget.validate()?;
                nonzero(revision)?;
                if width > i32::MAX as u32
                    || height > i32::MAX as u32
                    || (visible && (width == 0 || height == 0))
                {
                    return Err(FrameError::Bounds);
                }
            }
            Self::Remove { widget, revision } => {
                widget.validate()?;
                nonzero(revision)?;
            }
            Self::Dimming { revision, .. } => nonzero(revision)?,
            Self::NeedBaseline { widget } => widget.validate()?,
            Self::Unavailable { key, reason } => {
                if let Some(key) = key {
                    key.validate()?;
                }
                if !(1..=7).contains(&reason) {
                    return Err(FrameError::Invalid);
                }
            }
            Self::Retired { key } | Self::Registered { key } => key.validate()?,
            Self::Swapchain {
                generation,
                width,
                height,
                ready,
            } => {
                nonzero(generation)?;
                if width > i32::MAX as u32
                    || height > i32::MAX as u32
                    || (ready && (width == 0 || height == 0))
                {
                    return Err(FrameError::Bounds);
                }
            }
            Self::Progress {
                swapchain_generation,
                completed_draw,
                monotonic_ns,
            } => {
                nonzero(swapchain_generation)?;
                nonzero(completed_draw)?;
                nonzero(monotonic_ns)?;
            }
        }
        Ok(())
    }
}
fn nonzero(n: u64) -> Result<()> {
    if n == 0 {
        Err(FrameError::Invalid)
    } else {
        Ok(())
    }
}

pub const WIRE_HEADER_BYTES: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Envelope {
    pub kind: u16,
    pub payload_len: usize,
    pub fd_count: usize,
}
pub fn decode_envelope(header: &[u8]) -> Result<Envelope> {
    use crate::layout::take;
    if header.len() != WIRE_HEADER_BYTES || &header[..4] != b"UWFR" {
        return Err(FrameError::Invalid);
    }
    if u16::from_le_bytes(take(header, 4)?) != 1 {
        return Err(FrameError::Version);
    }
    let kind = u16::from_le_bytes(take(header, 6)?);
    let payload_len = u32::from_le_bytes(take(header, 8)?) as usize;
    let fd_count = u16::from_le_bytes(take(header, 12)?) as usize;
    if !(1..=14).contains(&kind)
        || payload_len > MAX_PAYLOAD
        || fd_count != if kind == 3 { 2 } else { 0 }
        || take::<2>(header, 14)? != [0; 2]
    {
        return Err(FrameError::Invalid);
    }
    Ok(Envelope {
        kind,
        payload_len,
        fd_count,
    })
}

#[derive(Default)]
struct Writer(Vec<u8>);
impl Writer {
    fn u16(&mut self, n: u16) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn u32(&mut self, n: u32) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn i32(&mut self, n: i32) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn boolean(&mut self, n: bool) {
        self.0.push(u8::from(n));
    }
    fn widget(&mut self, k: WidgetKey) {
        self.u32(k.id);
        self.u32(k.generation);
    }
    fn key(&mut self, k: AllocationKey) {
        self.0.extend_from_slice(&k.session.0);
        self.widget(k.widget);
        self.u64(k.generation);
    }
}
pub fn encode_message(message: &Message) -> Result<Vec<u8>> {
    message.validate()?;
    let mut p = Writer::default();
    let tag = match *message {
        Message::Hello {
            pid,
            start_ticks,
            alignment,
            mutex_size,
            mutex_align,
            abi,
            capability,
        } => {
            p.u32(pid);
            p.u64(start_ticks);
            p.u64(alignment);
            p.u16(mutex_size);
            p.u16(mutex_align);
            p.u32(abi);
            p.u32(capability);
            1
        }
        Message::Welcome {
            session,
            producer_pid,
            max_widgets,
            max_side,
            shared_budget,
        } => {
            p.0.extend_from_slice(&session.0);
            p.u32(producer_pid);
            p.u32(max_widgets);
            p.u32(max_side);
            p.u64(shared_budget);
            2
        }
        Message::Register(h) => {
            p.0.extend_from_slice(&h.encode()?);
            3
        }
        Message::Dirty { key, produced } => {
            p.key(key);
            p.u64(produced);
            4
        }
        Message::Consumed { key, consumed } => {
            p.key(key);
            p.u64(consumed);
            5
        }
        Message::Layout {
            widget,
            revision,
            x,
            y,
            width,
            height,
            z,
            visible,
        } => {
            p.widget(widget);
            p.u64(revision);
            p.i32(x);
            p.i32(y);
            p.u32(width);
            p.u32(height);
            p.i32(z);
            p.boolean(visible);
            6
        }
        Message::Remove { widget, revision } => {
            p.widget(widget);
            p.u64(revision);
            7
        }
        Message::Dimming { revision, active } => {
            p.u64(revision);
            p.boolean(active);
            8
        }
        Message::NeedBaseline { widget } => {
            p.widget(widget);
            9
        }
        Message::Unavailable { key, reason } => {
            if let Some(key) = key {
                p.key(key);
            } else {
                p.0.extend_from_slice(&[0; 32]);
            }
            p.u32(reason);
            10
        }
        Message::Retired { key } => {
            p.key(key);
            11
        }
        Message::Registered { key } => {
            p.key(key);
            12
        }
        Message::Swapchain {
            generation,
            width,
            height,
            ready,
        } => {
            p.u64(generation);
            p.u32(width);
            p.u32(height);
            p.boolean(ready);
            13
        }
        Message::Progress {
            swapchain_generation,
            completed_draw,
            monotonic_ns,
        } => {
            p.u64(swapchain_generation);
            p.u64(completed_draw);
            p.u64(monotonic_ns);
            14
        }
    };
    if p.0.len() > MAX_PAYLOAD {
        return Err(FrameError::Capacity);
    }
    let mut out = Writer::default();
    out.0.extend_from_slice(b"UWFR");
    out.u16(1);
    out.u16(tag);
    out.u32(p.0.len() as u32);
    out.u16(message.expected_fds() as u16);
    out.u16(0);
    out.0.extend_from_slice(&p.0);
    Ok(out.0)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let b = crate::layout::take(self.bytes, self.at)?;
        self.at = self.at.checked_add(N).ok_or(FrameError::Overflow)?;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.take::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(FrameError::Invalid),
        }
    }
    fn widget(&mut self) -> Result<WidgetKey> {
        Ok(WidgetKey {
            id: self.u32()?,
            generation: self.u32()?,
        })
    }
    fn key(&mut self) -> Result<AllocationKey> {
        Ok(AllocationKey {
            session: SessionId(self.take()?),
            widget: self.widget()?,
            generation: self.u64()?,
        })
    }
}
pub fn decode_message(bytes: &[u8]) -> Result<Message> {
    let header = bytes.get(..WIRE_HEADER_BYTES).ok_or(FrameError::Invalid)?;
    let envelope = decode_envelope(header)?;
    if bytes.len() != WIRE_HEADER_BYTES + envelope.payload_len {
        return Err(FrameError::Invalid);
    }
    let mut p = Reader {
        bytes: &bytes[WIRE_HEADER_BYTES..],
        at: 0,
    };
    let m = match envelope.kind {
        1 => Message::Hello {
            pid: p.u32()?,
            start_ticks: p.u64()?,
            alignment: p.u64()?,
            mutex_size: p.u16()?,
            mutex_align: p.u16()?,
            abi: p.u32()?,
            capability: p.u32()?,
        },
        2 => Message::Welcome {
            session: SessionId(p.take()?),
            producer_pid: p.u32()?,
            max_widgets: p.u32()?,
            max_side: p.u32()?,
            shared_budget: p.u64()?,
        },
        3 => Message::Register(AllocationHeader::decode(&p.take::<128>()?)?),
        4 => Message::Dirty {
            key: p.key()?,
            produced: p.u64()?,
        },
        5 => Message::Consumed {
            key: p.key()?,
            consumed: p.u64()?,
        },
        6 => Message::Layout {
            widget: p.widget()?,
            revision: p.u64()?,
            x: p.i32()?,
            y: p.i32()?,
            width: p.u32()?,
            height: p.u32()?,
            z: p.i32()?,
            visible: p.boolean()?,
        },
        7 => Message::Remove {
            widget: p.widget()?,
            revision: p.u64()?,
        },
        8 => Message::Dimming {
            revision: p.u64()?,
            active: p.boolean()?,
        },
        9 => Message::NeedBaseline {
            widget: p.widget()?,
        },
        10 => {
            let key = p.key()?;
            let key = if key.session.0 == [0; 16]
                && key.widget.id == 0
                && key.widget.generation == 0
                && key.generation == 0
            {
                None
            } else {
                Some(key)
            };
            Message::Unavailable {
                key,
                reason: p.u32()?,
            }
        }
        11 => Message::Retired { key: p.key()? },
        12 => Message::Registered { key: p.key()? },
        13 => Message::Swapchain {
            generation: p.u64()?,
            width: p.u32()?,
            height: p.u32()?,
            ready: p.boolean()?,
        },
        14 => Message::Progress {
            swapchain_generation: p.u64()?,
            completed_draw: p.u64()?,
            monotonic_ns: p.u64()?,
        },
        _ => return Err(FrameError::Invalid),
    };
    if p.at != p.bytes.len() {
        return Err(FrameError::Invalid);
    }
    m.validate()?;
    Ok(m)
}
