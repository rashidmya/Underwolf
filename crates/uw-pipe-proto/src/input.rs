//! Version 1 input messages. No native structs or pointers cross the wire.
use crate::pipe::ProtoError;
use serde::{Deserialize, Serialize};
pub const INPUT_VERSION: u16 = 1;
pub const MAX_BATCH_RECORDS: usize = 64;

macro_rules! codes {
    ($name:ident { $($variant:ident = $code:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all="snake_case")]
        #[repr(u8)]
        pub enum $name { $($variant=$code),* }
        impl $name { pub fn from_code(code:u8)->Result<Self,ProtoError> { match code { $($code=>Ok(Self::$variant),)* _=>Err(ProtoError::BadInput) } } }
    }
}
codes!(InputReason {Ready=0,Requested=1,Hotkey=2,FocusLost=3,Hidden=4,Disconnected=5,LeaseExpired=6,Overflow=7,WindowDestroyed=8,SetupFailed=9,Unavailable=10});
codes!(CaptureCode {Applied=0,StaleEpoch=1,Unavailable=2,SetupFailed=3,Superseded=4});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub kind: u8,
    pub flags: u8,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub modifiers: u32,
}
impl InputRecord {
    pub fn key(vk: i32, scan: i32, down: bool, extended: bool, repeat: bool, modifiers: u32) -> Self {
        Self {
            kind: 1,
            flags: down as u8 | (extended as u8) << 1 | (repeat as u8) << 2,
            a: vk,
            b: scan,
            c: 0,
            d: 0,
            modifiers,
        }
    }
    pub fn text(ch: char, modifiers: u32) -> Self {
        Self { kind: 2, flags: 0, a: ch as i32, b: 0, c: 0, d: 0, modifiers }
    }
    pub fn motion(x: i32, y: i32, modifiers: u32) -> Self {
        Self { kind: 3, flags: 0, a: x, b: y, c: 0, d: 0, modifiers }
    }
    pub fn button(button: i32, down: bool, x: i32, y: i32, modifiers: u32) -> Self {
        Self { kind: 4, flags: down as u8, a: button, b: x, c: y, d: 0, modifiers }
    }
    pub fn wheel(dx: i32, dy: i32, x: i32, y: i32, modifiers: u32) -> Self {
        Self { kind: 5, flags: 0, a: dx, b: dy, c: x, d: y, modifiers }
    }
    pub fn validate(&self) -> Result<(), ProtoError> {
        let valid = self.modifiers & !15 == 0
            && match self.kind {
                1 => {
                    (1..=255).contains(&self.a)
                        && (0..=65535).contains(&self.b)
                        && self.c == 0
                        && self.d == 0
                        && self.flags & !7 == 0
                }
                2 => {
                    char::from_u32(self.a as u32).is_some()
                        && self.b == 0
                        && self.c == 0
                        && self.d == 0
                        && self.flags == 0
                }
                3 => self.c == 0 && self.d == 0 && self.flags == 0,
                4 => (0..=4).contains(&self.a) && self.d == 0 && self.flags & !1 == 0,
                5 => self.flags == 0,
                _ => false,
            };
        if valid { Ok(()) } else { Err(ProtoError::BadInput) }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBatch {
    pub epoch: u64,
    pub first_seq: u64,
    pub records: Vec<InputRecord>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputState {
    pub epoch: u64,
    pub active: bool,
    pub reason: InputReason,
    pub state_seq: Option<u64>,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputChallenge {
    pub epoch: u64,
    pub nonce: u64,
    pub through_seq: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAck {
    pub epoch: u64,
    pub nonce: u64,
    pub through_seq: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub epoch: u64,
    pub state_seq: u64,
    pub request_id: u32,
    pub on: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureResult {
    pub epoch: u64,
    pub state_seq: u64,
    pub request_id: u32,
    pub code: CaptureCode,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMessage {
    Hello { module_version: u32, state_layout: u16, input_version: u16 },
    Welcome { input_version: u16 },
    State(InputState),
    Batch(InputBatch),
    Challenge(InputChallenge),
    Ack(InputAck),
    Capture(CaptureRequest),
    Result(CaptureResult),
}
fn nonzero(n: u64) -> Result<(), ProtoError> {
    if n == 0 { Err(ProtoError::BadInput) } else { Ok(()) }
}
fn put(out: &mut Vec<u8>, bytes: impl AsRef<[u8]>) {
    out.extend_from_slice(bytes.as_ref());
}
impl InputMessage {
    pub fn validate(&self) -> Result<(), ProtoError> {
        match self {
            Self::Hello { .. } | Self::Welcome { .. } => Ok(()),
            Self::State(s) => nonzero(s.epoch),
            Self::Batch(b) => {
                nonzero(b.epoch)?;
                nonzero(b.first_seq)?;
                if b.records.is_empty()
                    || b.records.len() > MAX_BATCH_RECORDS
                    || b.first_seq.checked_add(b.records.len() as u64 - 1).is_none()
                {
                    return Err(ProtoError::BadInput);
                }
                b.records.iter().try_for_each(InputRecord::validate)
            }
            Self::Challenge(c) => {
                nonzero(c.epoch)?;
                nonzero(c.nonce)
            }
            Self::Ack(c) => {
                nonzero(c.epoch)?;
                nonzero(c.nonce)
            }
            Self::Capture(c) => nonzero(c.epoch),
            Self::Result(c) => nonzero(c.epoch),
        }
    }
    pub(crate) fn encode(&self) -> Result<(u16, Vec<u8>), ProtoError> {
        self.validate()?;
        let mut p = Vec::new();
        let tag = match self {
            Self::Hello { module_version, state_layout, input_version } => {
                put(&mut p, module_version.to_le_bytes());
                put(&mut p, state_layout.to_le_bytes());
                put(&mut p, input_version.to_le_bytes());
                0x110
            }
            Self::Welcome { input_version } => {
                put(&mut p, input_version.to_le_bytes());
                put(&mut p, [0; 2]);
                0x111
            }
            Self::State(s) => {
                put(&mut p, s.epoch.to_le_bytes());
                put(&mut p, [s.active as u8, s.reason as u8, s.state_seq.is_some() as u8, 0]);
                put(&mut p, s.state_seq.unwrap_or(0).to_le_bytes());
                put(&mut p, s.width.to_le_bytes());
                put(&mut p, s.height.to_le_bytes());
                0x112
            }
            Self::Batch(b) => {
                put(&mut p, b.epoch.to_le_bytes());
                put(&mut p, b.first_seq.to_le_bytes());
                put(&mut p, (b.records.len() as u16).to_le_bytes());
                put(&mut p, [0; 2]);
                for r in &b.records {
                    put(&mut p, [r.kind, r.flags, 0, 0]);
                    for n in [r.a, r.b, r.c, r.d] {
                        put(&mut p, n.to_le_bytes());
                    }
                    put(&mut p, r.modifiers.to_le_bytes());
                }
                0x113
            }
            Self::Challenge(c) => {
                for n in [c.epoch, c.nonce, c.through_seq] {
                    put(&mut p, n.to_le_bytes())
                }
                0x114
            }
            Self::Ack(c) => {
                for n in [c.epoch, c.nonce, c.through_seq] {
                    put(&mut p, n.to_le_bytes())
                }
                0x115
            }
            Self::Capture(c) => {
                put(&mut p, c.epoch.to_le_bytes());
                put(&mut p, c.state_seq.to_le_bytes());
                put(&mut p, c.request_id.to_le_bytes());
                put(&mut p, [c.on as u8, 0, 0, 0]);
                0x116
            }
            Self::Result(c) => {
                put(&mut p, c.epoch.to_le_bytes());
                put(&mut p, c.state_seq.to_le_bytes());
                put(&mut p, c.request_id.to_le_bytes());
                put(&mut p, (c.code as u16).to_le_bytes());
                put(&mut p, [0; 2]);
                0x117
            }
        };
        Ok((tag, p))
    }
    pub(crate) fn decode(tag: u16, p: &[u8]) -> Result<Self, ProtoError> {
        let mut c = Reader { bytes: p, at: 0 };
        let m = match tag {
            0x110 => Self::Hello { module_version: c.u32()?, state_layout: c.u16()?, input_version: c.u16()? },
            0x111 => {
                let input_version = c.u16()?;
                c.zero(2)?;
                Self::Welcome { input_version }
            }
            0x112 => {
                let epoch = c.u64()?;
                let active = c.boolean()?;
                let reason = InputReason::from_code(c.u8()?)?;
                let has = c.boolean()?;
                c.zero(1)?;
                let seq = c.u64()?;
                if !has && seq != 0 {
                    return Err(ProtoError::BadInput);
                }
                Self::State(InputState {
                    epoch,
                    active,
                    reason,
                    state_seq: has.then_some(seq),
                    width: c.u32()?,
                    height: c.u32()?,
                })
            }
            0x113 => {
                let epoch = c.u64()?;
                let first_seq = c.u64()?;
                let count = c.u16()? as usize;
                c.zero(2)?;
                if count == 0 || count > MAX_BATCH_RECORDS {
                    return Err(ProtoError::BadInput);
                }
                let mut records = Vec::with_capacity(count);
                for _ in 0..count {
                    let kind = c.u8()?;
                    let flags = c.u8()?;
                    c.zero(2)?;
                    records.push(InputRecord {
                        kind,
                        flags,
                        a: c.u32()? as i32,
                        b: c.u32()? as i32,
                        c: c.u32()? as i32,
                        d: c.u32()? as i32,
                        modifiers: c.u32()?,
                    });
                }
                Self::Batch(InputBatch { epoch, first_seq, records })
            }
            0x114 => Self::Challenge(InputChallenge { epoch: c.u64()?, nonce: c.u64()?, through_seq: c.u64()? }),
            0x115 => Self::Ack(InputAck { epoch: c.u64()?, nonce: c.u64()?, through_seq: c.u64()? }),
            0x116 => {
                let r = CaptureRequest { epoch: c.u64()?, state_seq: c.u64()?, request_id: c.u32()?, on: c.boolean()? };
                c.zero(3)?;
                Self::Capture(r)
            }
            0x117 => {
                let epoch = c.u64()?;
                let state_seq = c.u64()?;
                let request_id = c.u32()?;
                let code = c.u16()?;
                if code > u8::MAX as u16 {
                    return Err(ProtoError::BadInput);
                }
                c.zero(2)?;
                Self::Result(CaptureResult { epoch, state_seq, request_id, code: CaptureCode::from_code(code as u8)? })
            }
            _ => return Err(ProtoError::UnknownType(tag)),
        };
        if c.at != p.len() {
            return Err(ProtoError::BadInput);
        }
        m.validate()?;
        Ok(m)
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], ProtoError> {
        let slice = self.bytes.get(self.at..self.at + N).ok_or(ProtoError::Truncated)?;
        self.at += N;
        Ok(slice.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn boolean(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtoError::BadInput),
        }
    }
    fn zero(&mut self, n: usize) -> Result<(), ProtoError> {
        for _ in 0..n {
            if self.u8()? != 0 {
                return Err(ProtoError::BadInput);
            }
        }
        Ok(())
    }
}
