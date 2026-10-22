//! Fixed-layout, little-endian binary messages (spec 6.3). 16-byte header
//! (`magic`="UWP1", `type: u16`, `version: u16`, `pid: u32`, `payload_len: u32`) then a payload of
//! at most `MAX_PAYLOAD` bytes. Paths are UTF-16 with a `u16` code-unit-count prefix (<= 1024);
//! status messages are UTF-8 with a `u16` byte-count prefix (<= 256).

pub const MAGIC: [u8; 4] = *b"UWP1";
pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;
pub const MAX_PAYLOAD: usize = 4096;
pub const MAX_PATH_UNITS: usize = 1024;
pub const MAX_STATUS_BYTES: usize = 256;

const TYPE_HELLO: u16 = 1;
const TYPE_LOAD_OVERLAY: u16 = 2;
const TYPE_STATUS: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    AlreadyLoaded,
    AlreadyLoadedOther,
    LoadFailed,
    NoStartExport,
    StartFailed,
    Unsupported,
}

impl StatusCode {
    pub fn to_u16(self) -> u16 {
        match self {
            StatusCode::Ok => 0,
            StatusCode::AlreadyLoaded => 1,
            StatusCode::AlreadyLoadedOther => 2,
            StatusCode::LoadFailed => 3,
            StatusCode::NoStartExport => 4,
            StatusCode::StartFailed => 5,
            StatusCode::Unsupported => 6,
        }
    }
    pub fn from_u16(v: u16) -> Option<StatusCode> {
        Some(match v {
            0 => StatusCode::Ok,
            1 => StatusCode::AlreadyLoaded,
            2 => StatusCode::AlreadyLoadedOther,
            3 => StatusCode::LoadFailed,
            4 => StatusCode::NoStartExport,
            5 => StatusCode::StartFailed,
            6 => StatusCode::Unsupported,
            _ => return None,
        })
    }
    /// The lowercase wire name used in the JSON-lines `status` event.
    pub fn as_str(self) -> &'static str {
        match self {
            StatusCode::Ok => "ok",
            StatusCode::AlreadyLoaded => "already_loaded",
            StatusCode::AlreadyLoadedOther => "already_loaded_other",
            StatusCode::LoadFailed => "load_failed",
            StatusCode::NoStartExport => "no_start_export",
            StatusCode::StartFailed => "start_failed",
            StatusCode::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// bootstrap -> helper, always first.
    Hello {
        protocol_version: u16,
        bootstrap_version: u16,
        bitness: u16,
        overlay_loaded: bool,
        overlay_path: String,
    },
    /// helper -> bootstrap.
    LoadOverlay { request_id: u32, path: String },
    /// bootstrap -> helper.
    Status { request_id: u32, code: StatusCode, detail: u32, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    Short,
    BadMagic,
    /// The header's `version` is not `PROTOCOL_VERSION` (carries the version seen).
    BadVersion(u16),
    PayloadTooLarge,
    StringTooLong,
    BadString,
    /// A `status` with a code outside `StatusCode` (carries the raw code).
    BadCode(u16),
    UnknownType(u16),
    Truncated,
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Short => write!(f, "message shorter than the header"),
            ProtoError::BadMagic => write!(f, "bad magic"),
            ProtoError::BadVersion(v) => write!(f, "unsupported header version {v}"),
            ProtoError::PayloadTooLarge => write!(f, "payload over {MAX_PAYLOAD} bytes"),
            ProtoError::StringTooLong => write!(f, "string over its limit"),
            ProtoError::BadString => write!(f, "invalid string bytes"),
            ProtoError::BadCode(c) => write!(f, "unknown status code {c}"),
            ProtoError::UnknownType(t) => write!(f, "unknown message type {t}"),
            ProtoError::Truncated => write!(f, "payload shorter than its fields"),
        }
    }
}
impl std::error::Error for ProtoError {}

/// A path is capped at `MAX_PATH_UNITS` code units only as a last-resort floor. Callers that could
/// supply a longer path (the helper's `load_overlay`) must reject it first and report an error
/// (`path_too_long`), so a path is never silently truncated into a different, loadable path.
fn put_path(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    let n = units.len().min(MAX_PATH_UNITS);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    for u in &units[..n] {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

fn put_status_msg(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(MAX_STATUS_BYTES);
    // Do not split a UTF-8 code point at the cap.
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.extend_from_slice(&(end as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..end]);
}

/// Encode a message into a framed buffer (header + payload).
pub fn encode(m: &Message) -> Vec<u8> {
    let (ty, payload) = match m {
        Message::Hello { protocol_version, bootstrap_version, bitness, overlay_loaded, overlay_path } => {
            let mut p = Vec::new();
            p.extend_from_slice(&protocol_version.to_le_bytes());
            p.extend_from_slice(&bootstrap_version.to_le_bytes());
            p.extend_from_slice(&bitness.to_le_bytes());
            p.push(*overlay_loaded as u8);
            put_path(&mut p, overlay_path);
            (TYPE_HELLO, p)
        }
        Message::LoadOverlay { request_id, path } => {
            let mut p = Vec::new();
            p.extend_from_slice(&request_id.to_le_bytes());
            put_path(&mut p, path);
            (TYPE_LOAD_OVERLAY, p)
        }
        Message::Status { request_id, code, detail, message } => {
            let mut p = Vec::new();
            p.extend_from_slice(&request_id.to_le_bytes());
            p.extend_from_slice(&code.to_u16().to_le_bytes());
            p.extend_from_slice(&detail.to_le_bytes());
            put_status_msg(&mut p, message);
            (TYPE_STATUS, p)
        }
    };
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    // `pid` is reserved (the sender's Windows pid); 0 here, filled and checked by plan 2.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Cur<'a> {
    fn u16(&mut self) -> Result<u16, ProtoError> {
        let end = self.i + 2;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        let end = self.i + 4;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        let v = *self.b.get(self.i).ok_or(ProtoError::Truncated)?;
        self.i += 1;
        Ok(v)
    }
    fn path(&mut self) -> Result<String, ProtoError> {
        let n = self.u16()? as usize;
        if n > MAX_PATH_UNITS {
            return Err(ProtoError::StringTooLong);
        }
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            units.push(self.u16()?);
        }
        String::from_utf16(&units).map_err(|_| ProtoError::BadString)
    }
    fn status_msg(&mut self) -> Result<String, ProtoError> {
        let n = self.u16()? as usize;
        if n > MAX_STATUS_BYTES {
            return Err(ProtoError::StringTooLong);
        }
        let end = self.i + n;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        String::from_utf8(s.to_vec()).map_err(|_| ProtoError::BadString)
    }
}

/// Decode one framed message. A string-length field over its cap fails before any allocation.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtoError::Short);
    }
    if bytes[0..4] != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    let ty = u16::from_le_bytes([bytes[4], bytes[5]]);
    // A different header version means a different wire format: reject it before reading the
    // body. The helper maps `BadVersion` on a hello to `version_mismatch` (see `decide`).
    let version = u16::from_le_bytes([bytes[6], bytes[7]]);
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::BadVersion(version));
    }
    // bytes[8..12] pid — reserved, not needed to decode the body here.
    let payload_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(ProtoError::PayloadTooLarge);
    }
    let payload = bytes.get(HEADER_LEN..HEADER_LEN + payload_len).ok_or(ProtoError::Truncated)?;
    let mut c = Cur { b: payload, i: 0 };
    match ty {
        TYPE_HELLO => Ok(Message::Hello {
            protocol_version: c.u16()?,
            bootstrap_version: c.u16()?,
            bitness: c.u16()?,
            overlay_loaded: c.u8()? != 0,
            overlay_path: c.path()?,
        }),
        TYPE_LOAD_OVERLAY => Ok(Message::LoadOverlay { request_id: c.u32()?, path: c.path()? }),
        TYPE_STATUS => {
            let request_id = c.u32()?;
            let raw = c.u16()?;
            let code = StatusCode::from_u16(raw).ok_or(ProtoError::BadCode(raw))?;
            let detail = c.u32()?;
            let message = c.status_msg()?;
            Ok(Message::Status { request_id, code, detail, message })
        }
        other => Err(ProtoError::UnknownType(other)),
    }
}
