//! Socket protocol between the Electron host (server) and the Vulkan layer (client).
//!
//! Wire format: `u32` LE payload length, then a `postcard` payload. File descriptors travel as
//! SCM_RIGHTS on the same `sendmsg`, so they arrive with the first byte of their message.

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::io::{IoSlice, IoSliceMut};
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::fd::{BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

#[cfg(unix)]
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
    recvmsg, sendmsg,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 1;
/// Upper bound on fds per message (DRM buffers have at most 4 planes).
pub const MAX_FDS: usize = 4;
const MAX_PAYLOAD: u32 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plane {
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Swapchain pixels.
    MouseMove { x: f32, y: f32 },
    /// 0 = left, 1 = middle, 2 = right.
    MouseButton { button: u8, down: bool },
    Wheel { dx: f32, dy: f32 },
    /// X11 keycode; `text` for printable input.
    Key { code: u32, down: bool, text: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    /// layer → host, sent once after connecting.
    Hello { protocol: u32, pid: u32 },
    /// layer → host, on swapchain create/recreate.
    Swapchain { width: u32, height: u32 },
    /// host → layer, with `planes.len()` dmabuf fds attached.
    Frame {
        id: u64,
        widget: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        drm_fourcc: u32,
        modifier: u64,
        planes: Vec<Plane>,
    },
    /// layer → host, after the GPU copy of frame `id` completed.
    Ack { id: u64 },
    /// host → layer.
    Layout { widget: u32, x: i32, y: i32, visible: bool },
    /// host → layer, with one memfd attached: a shared-memory slot holding CPU-rendered widget
    /// pixels. Sent once per slot per connection, and again if the widget's size changes.
    Slot { widget: u32, slot: u32, width: u32, height: u32, stride: u32, drm_fourcc: u32 },
    /// host → layer: slot `slot` of `widget` holds a new frame. Ack after the GPU copy completed.
    FrameShm { id: u64, widget: u32, slot: u32, x: i32, y: i32 },
    /// Input for the overlay, from whichever side captured it (Spike 1).
    Input(InputEvent),
    /// layer → host: the edit-mode hotkey asks for this state. host → layer: the current state.
    EditMode { on: bool },
}

/// Window coordinates → swapchain pixels (the game may render at a different size than its
/// window, e.g. with render scaling).
pub fn window_to_swapchain(p: (f32, f32), window: (u32, u32), swapchain: (u32, u32)) -> (f32, f32) {
    if window.0 == 0 || window.1 == 0 {
        return (0.0, 0.0);
    }
    (p.0 * swapchain.0 as f32 / window.0 as f32, p.1 * swapchain.1 as f32 / window.1 as f32)
}

/// Frame a message as a `u32` LE payload length followed by its `postcard` payload into any
/// writer. The C3 Win32 hook (a Windows PE under Wine) and the host share this over localhost
/// TCP; only the transport differs from the Unix-socket `send`/`recv` path.
pub fn write_msg<W: Write>(w: &mut W, m: &Msg) -> io::Result<()> {
    let payload = postcard::to_stdvec(m).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&n| n <= MAX_PAYLOAD)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "message too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&payload)?;
    Ok(())
}

/// Read one framed message from any reader. Returns `UnexpectedEof` on a clean peer close.
pub fn read_msg<R: Read>(r: &mut R) -> io::Result<Msg> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("payload too large: {len}")));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    postcard::from_bytes(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(unix)]
pub fn send(s: &UnixStream, m: &Msg, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    if fds.len() > MAX_FDS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many fds"));
    }
    let payload = postcard::to_stdvec(m).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&payload);

    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::Error::other("ancillary buffer too small"));
    }
    let mut sent = sendmsg(s, &[IoSlice::new(&buf)], &mut control, SendFlags::NOSIGNAL)?;
    // Only the first chunk carries the fds; finish any short write with plain sends.
    let mut control = SendAncillaryBuffer::default();
    while sent < buf.len() {
        sent += sendmsg(s, &[IoSlice::new(&buf[sent..])], &mut control, SendFlags::NOSIGNAL)?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn recv(s: &UnixStream) -> io::Result<(Msg, Vec<OwnedFd>)> {
    let mut header = [0u8; 4];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let got = recvmsg(s, &mut [IoSliceMut::new(&mut header)], &mut control, RecvFlags::CMSG_CLOEXEC)?.bytes;
    if got == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let mut fds = Vec::new();
    for msg in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = msg {
            fds.extend(rights);
        }
    }
    let mut reader = s;
    reader.read_exact(&mut header[got..])?;
    let len = u32::from_le_bytes(header);
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("payload too large: {len}")));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;
    let msg = postcard::from_bytes(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((msg, fds))
}

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
}

pub const DRM_FORMAT_ARGB8888: u32 = fourcc(b'A', b'R', b'2', b'4');
pub const DRM_FORMAT_ABGR8888: u32 = fourcc(b'A', b'B', b'2', b'4');

/// Electron `textureInfo.pixelFormat` → DRM fourcc. `bgra` is B,G,R,A in memory, which DRM
/// (little-endian packed) calls ARGB8888.
pub fn fourcc_for(pixel_format: &str) -> Option<u32> {
    match pixel_format {
        "bgra" => Some(DRM_FORMAT_ARGB8888),
        "rgba" => Some(DRM_FORMAT_ABGR8888),
        _ => None,
    }
}

/// Electron reports the DRM modifier as a decimal string (a u64 doesn't fit a JS number).
pub fn parse_modifier(s: &str) -> Option<u64> {
    s.parse().ok()
}
