//! Host side of the overlay socket: accepts one layer connection at a time and hands it
//! Electron's shared-texture frames. Every `send_frame` resolves quickly (ack, timeout or
//! drop) so the host can always release the texture back to Chromium's small frame pool.

#[cfg(feature = "node")]
mod node;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, RawFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::net::{TcpListener, TcpStream};

use uw_spike_ipc::{InputEvent, Msg, Plane, fourcc_for, parse_modifier, read_msg, recv, send};

pub struct PlaneFd {
    /// Owned by Electron; only borrowed for the duration of `send_frame`.
    pub fd: RawFd,
    pub stride: u32,
    pub offset: u32,
    pub size: u32,
}

pub struct FrameInfo {
    pub widget: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub modifier: String,
    pub planes: Vec<PlaneFd>,
}

/// A CPU-rendered widget frame (Electron's offscreen `NativeImage` bitmap).
pub struct Bitmap<'a> {
    pub widget: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: &'a str,
    pub data: &'a [u8],
}

/// Shared-memory slots per widget. A slot is busy from send until ack or timeout.
pub const SLOTS_PER_WIDGET: usize = 3;
const PAGE: u64 = 4096;

struct SlotMem {
    file: File,
    busy: bool,
}

struct WidgetSlots {
    width: u32,
    height: u32,
    stride: u32,
    drm_fourcc: u32,
    slots: Vec<SlotMem>,
}

impl WidgetSlots {
    fn new(width: u32, height: u32, stride: u32, drm_fourcc: u32) -> io::Result<WidgetSlots> {
        // Page-aligned so the layer can import the mapping with VK_EXT_external_memory_host.
        let len = (stride as u64 * height as u64).div_ceil(PAGE) * PAGE;
        let slots = (0..SLOTS_PER_WIDGET)
            .map(|_| {
                let fd = rustix::fs::memfd_create("uw-widget-slot", rustix::fs::MemfdFlags::CLOEXEC)?;
                let file = File::from(fd);
                file.set_len(len)?;
                Ok(SlotMem { file, busy: false })
            })
            .collect::<io::Result<_>>()?;
        Ok(WidgetSlots { width, height, stride, drm_fourcc, slots })
    }

    fn matches(&self, b: &Bitmap<'_>, drm_fourcc: u32) -> bool {
        (self.width, self.height, self.stride, self.drm_fourcc) == (b.width, b.height, b.stride, drm_fourcc)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    Acked,
    Dropped,
    Timeout,
}

impl SendResult {
    pub fn as_str(self) -> &'static str {
        match self {
            SendResult::Acked => "acked",
            SendResult::Dropped => "dropped",
            SendResult::Timeout => "timeout",
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<()>>>>;

struct Peer {
    generation: u64,
    /// Serializes writes so concurrent frames never interleave on the socket.
    writer: Arc<Mutex<UnixStream>>,
    pending: Pending,
    /// (widget, slot) pairs whose memfd this peer already received.
    registered: Arc<Mutex<HashSet<(u32, u32)>>>,
}

#[derive(Default)]
struct Inner {
    peer: Mutex<Option<Peer>>,
    /// Edit-mode states the layer's hotkey asked for, oldest first.
    edit_requests: Mutex<Vec<bool>>,
    /// Overlay input captured by the layer (candidate C2), oldest first.
    inputs: Mutex<Vec<InputEvent>>,
    swapchain: Mutex<Option<(u32, u32)>>,
    widgets: Mutex<HashMap<u32, WidgetSlots>>,
    next_id: AtomicU64,
    next_generation: AtomicU64,
    connections: AtomicU64,
}

pub struct Server {
    inner: Arc<Inner>,
}

impl Server {
    /// Binds the socket (replacing a stale file) and starts accepting layer connections.
    /// A new connection replaces the previous peer.
    pub fn bind(path: &Path) -> io::Result<Server> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = UnixListener::bind(path)?;
        let inner = Arc::new(Inner::default());
        let accept_inner = inner.clone();
        std::thread::Builder::new()
            .name("uw-bridge-accept".into())
            .spawn(move || accept_loop(listener, accept_inner))?;
        Ok(Server { inner })
    }

    /// Number of layer connections accepted so far; changes whenever a new layer connects
    /// (the host then repaints every widget, since the new layer has none of its frames).
    pub fn peer_generation(&self) -> u64 {
        self.inner.connections.load(Ordering::Relaxed)
    }

    pub fn pending_edit_requests(&self) -> usize {
        self.inner.edit_requests.lock().unwrap().len()
    }

    pub fn pending_inputs(&self) -> usize {
        self.inner.inputs.lock().unwrap().len()
    }

    pub fn take_inputs(&self) -> Vec<InputEvent> {
        std::mem::take(&mut *self.inner.inputs.lock().unwrap())
    }

    pub fn take_edit_requests(&self) -> Vec<bool> {
        std::mem::take(&mut *self.inner.edit_requests.lock().unwrap())
    }

    /// The game's swapchain size, as last reported by the layer.
    pub fn swapchain_size(&self) -> Option<(u32, u32)> {
        *self.inner.swapchain.lock().unwrap()
    }

    /// Listens for the C3 Win32 hook (candidate C3), which runs inside the game under Proton and
    /// forwards edit-mode toggles and captured input over localhost TCP. Its messages feed the same
    /// `edit_requests`/`inputs` queues as the layer's C2 path, so the host routes them identically.
    pub fn bind_input_tcp(&self, port: u16) -> io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("uw-bridge-tcp".into())
            .spawn(move || tcp_accept_loop(listener, inner))?;
        Ok(())
    }

    /// Tells the layer where to draw the edit-mode cursor (swapchain px). Used by the C3 path,
    /// where the cursor is accumulated host-side from the hook's relative mouse motion. False when
    /// no layer is connected.
    pub fn set_cursor(&self, x: f32, y: f32) -> bool {
        self.control(&Msg::Input(InputEvent::MouseMove { x, y }))
    }

    /// Tells the layer the current edit-mode state. False when no layer is connected.
    pub fn set_edit_mode(&self, on: bool) -> bool {
        self.control(&Msg::EditMode { on })
    }

    /// Moves or hides a widget on the layer side (static widgets send no new frames).
    pub fn set_layout(&self, widget: u32, x: i32, y: i32, visible: bool) -> bool {
        self.control(&Msg::Layout { widget, x, y, visible })
    }

    fn control(&self, msg: &Msg) -> bool {
        let writer = match &*self.inner.peer.lock().unwrap() {
            Some(p) => p.writer.clone(),
            None => return false,
        };
        send(&writer.lock().unwrap(), msg, &[]).is_ok()
    }

    pub fn has_peer(&self) -> bool {
        self.inner.peer.lock().unwrap().is_some()
    }

    pub fn send_frame(&self, f: &FrameInfo, timeout: Duration) -> SendResult {
        let (Some(drm_fourcc), Some(modifier)) = (fourcc_for(&f.pixel_format), parse_modifier(&f.modifier)) else {
            return SendResult::Dropped;
        };
        let (writer, pending) = match &*self.inner.peer.lock().unwrap() {
            Some(p) => (p.writer.clone(), p.pending.clone()),
            None => return SendResult::Dropped,
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        pending.lock().unwrap().insert(id, tx);

        let msg = Msg::Frame {
            id,
            widget: f.widget,
            x: f.x,
            y: f.y,
            width: f.width,
            height: f.height,
            drm_fourcc,
            modifier,
            planes: f.planes.iter().map(|p| Plane { offset: p.offset, stride: p.stride }).collect(),
        };
        // SAFETY: Electron keeps these fds open until the texture is released, which the
        // caller does only after this returns.
        let fds: Vec<BorrowedFd<'_>> = f.planes.iter().map(|p| unsafe { BorrowedFd::borrow_raw(p.fd) }).collect();
        if send(&writer.lock().unwrap(), &msg, &fds).is_err() {
            pending.lock().unwrap().remove(&id);
            return SendResult::Dropped;
        }
        wait_ack(&rx, &pending, id, timeout)
    }
}

impl Server {
    /// Copies `b` into a free shared-memory slot of its widget (the only CPU copy), tells the
    /// layer, and waits for the ack. Returns `Dropped` at once when no layer is connected, the
    /// format is unsupported, or all of the widget's slots are busy.
    pub fn send_bitmap(&self, b: &Bitmap<'_>, timeout: Duration) -> SendResult {
        let Some(drm_fourcc) = fourcc_for(b.pixel_format) else {
            return SendResult::Dropped;
        };
        let size = b.stride as usize * b.height as usize;
        if b.data.len() < size || (b.stride as u64) < b.width as u64 * 4 {
            return SendResult::Dropped;
        }
        let (writer, pending, registered) = match &*self.inner.peer.lock().unwrap() {
            Some(p) => (p.writer.clone(), p.pending.clone(), p.registered.clone()),
            None => return SendResult::Dropped,
        };

        let (slot, file) = {
            let mut widgets = self.inner.widgets.lock().unwrap();
            let resized = widgets.get(&b.widget).is_none_or(|w| !w.matches(b, drm_fourcc));
            if resized {
                if widgets.get(&b.widget).is_some_and(|w| w.slots.iter().any(|s| s.busy)) {
                    return SendResult::Dropped;
                }
                let Ok(fresh) = WidgetSlots::new(b.width, b.height, b.stride, drm_fourcc) else {
                    return SendResult::Dropped;
                };
                widgets.insert(b.widget, fresh);
                registered.lock().unwrap().retain(|&(w, _)| w != b.widget);
            }
            let w = widgets.get_mut(&b.widget).expect("inserted above");
            let Some(slot) = w.slots.iter().position(|s| !s.busy) else {
                return SendResult::Dropped;
            };
            let Ok(file) = w.slots[slot].file.try_clone() else {
                return SendResult::Dropped;
            };
            w.slots[slot].busy = true;
            (slot as u32, file)
        };

        let result = (|| {
            file.write_all_at(&b.data[..size], 0).map_err(|_| SendResult::Dropped)?;
            let first_use = registered.lock().unwrap().insert((b.widget, slot));
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = mpsc::channel();
            pending.lock().unwrap().insert(id, tx);
            let sent = {
                let w = writer.lock().unwrap();
                let slot_msg = Msg::Slot {
                    widget: b.widget,
                    slot,
                    width: b.width,
                    height: b.height,
                    stride: b.stride,
                    drm_fourcc,
                };
                (!first_use || send(&w, &slot_msg, &[file.as_fd()]).is_ok())
                    && send(&w, &Msg::FrameShm { id, widget: b.widget, slot, x: b.x, y: b.y }, &[]).is_ok()
            };
            if !sent {
                pending.lock().unwrap().remove(&id);
                registered.lock().unwrap().remove(&(b.widget, slot));
                return Err(SendResult::Dropped);
            }
            Ok(wait_ack(&rx, &pending, id, timeout))
        })()
        .unwrap_or_else(|e| e);

        if let Some(w) = self.inner.widgets.lock().unwrap().get_mut(&b.widget)
            && let Some(s) = w.slots.get_mut(slot as usize)
        {
            s.busy = false;
        }
        result
    }
}

fn wait_ack(rx: &mpsc::Receiver<()>, pending: &Pending, id: u64, timeout: Duration) -> SendResult {
    match rx.recv_timeout(timeout) {
        Ok(()) => SendResult::Acked,
        Err(RecvTimeoutError::Timeout) => {
            pending.lock().unwrap().remove(&id);
            SendResult::Timeout
        }
        Err(RecvTimeoutError::Disconnected) => SendResult::Dropped,
    }
}

fn accept_loop(listener: UnixListener, inner: Arc<Inner>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let Ok(reader) = stream.try_clone() else { continue };
        let generation = inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let pending: Pending = Arc::default();
        let previous = inner.peer.lock().unwrap().replace(Peer {
            generation,
            writer: Arc::new(Mutex::new(stream)),
            pending: pending.clone(),
            registered: Arc::default(),
        });
        if let Some(old) = previous {
            let _ = old.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
        inner.connections.fetch_add(1, Ordering::Relaxed);
        let reader_inner = inner.clone();
        let _ = std::thread::Builder::new()
            .name("uw-bridge-peer".into())
            .spawn(move || read_loop(reader, generation, pending, reader_inner));
    }
}

fn tcp_accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let inner = inner.clone();
        let _ =
            std::thread::Builder::new().name("uw-bridge-tcp-peer".into()).spawn(move || tcp_read_loop(stream, inner));
    }
}

/// Reads the C3 hook's framed messages. Edit-mode toggles and input go into the shared queues the
/// host already drains. If the hook dies while edit mode is on, leave edit mode so the game can't
/// get stuck frozen.
fn tcp_read_loop(mut stream: TcpStream, inner: Arc<Inner>) {
    let mut edit_on = false;
    while let Ok(msg) = read_msg(&mut stream) {
        match msg {
            Msg::EditMode { on } => {
                edit_on = on;
                inner.edit_requests.lock().unwrap().push(on);
            }
            Msg::Input(ev) => inner.inputs.lock().unwrap().push(ev),
            _ => {}
        }
    }
    if edit_on {
        inner.edit_requests.lock().unwrap().push(false);
    }
}

fn read_loop(stream: UnixStream, generation: u64, pending: Pending, inner: Arc<Inner>) {
    while let Ok((msg, _fds)) = recv(&stream) {
        match msg {
            Msg::Ack { id } => {
                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(());
                }
            }
            Msg::EditMode { on } => inner.edit_requests.lock().unwrap().push(on),
            Msg::Input(ev) => inner.inputs.lock().unwrap().push(ev),
            Msg::Swapchain { width, height } => *inner.swapchain.lock().unwrap() = Some((width, height)),
            _ => {}
        }
    }
    let mut peer = inner.peer.lock().unwrap();
    if peer.as_ref().is_some_and(|p| p.generation == generation) {
        *peer = None;
    }
    drop(peer);
    // Waiters see Disconnected and report Dropped.
    pending.lock().unwrap().clear();
}
