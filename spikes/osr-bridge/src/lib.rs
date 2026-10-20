//! Host side of the overlay socket: accepts one layer connection at a time and hands it
//! Electron's shared-texture frames. Every `send_frame` resolves quickly (ack, timeout or
//! drop) so the host can always release the texture back to Chromium's small frame pool.

#[cfg(feature = "node")]
mod node;

use std::collections::HashMap;
use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uw_spike_ipc::{Msg, Plane, fourcc_for, parse_modifier, recv, send};

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
}

#[derive(Default)]
struct Inner {
    peer: Mutex<Option<Peer>>,
    next_id: AtomicU64,
    next_generation: AtomicU64,
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
        match rx.recv_timeout(timeout) {
            Ok(()) => SendResult::Acked,
            Err(RecvTimeoutError::Timeout) => {
                pending.lock().unwrap().remove(&id);
                SendResult::Timeout
            }
            Err(RecvTimeoutError::Disconnected) => SendResult::Dropped,
        }
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
        });
        if let Some(old) = previous {
            let _ = old.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
        let reader_inner = inner.clone();
        let _ = std::thread::Builder::new()
            .name("uw-bridge-peer".into())
            .spawn(move || read_loop(reader, generation, pending, reader_inner));
    }
}

fn read_loop(stream: UnixStream, generation: u64, pending: Pending, inner: Arc<Inner>) {
    while let Ok((msg, _fds)) = recv(&stream) {
        if let Msg::Ack { id } = msg
            && let Some(tx) = pending.lock().unwrap().remove(&id)
        {
            let _ = tx.send(());
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
