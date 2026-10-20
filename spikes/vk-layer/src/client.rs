//! Background connection to the Electron host. Runs on its own threads and never blocks the
//! present path: the present hook only takes queued frames (`take_frames`) and queues acks.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{LazyLock, Mutex, Once};
use std::time::Duration;

use uw_spike_input::capture_wine_x11;
use uw_spike_input::hotkey_xi2::{self, HotkeyState, RawInput, key_text};
use uw_spike_ipc::{InputEvent, Msg, PROTOCOL, recv, send};

use crate::present::spike_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFrame {
    pub id: u64,
    pub widget: u32,
    pub slot: u32,
    pub x: i32,
    pub y: i32,
    /// Already acked (outside composite mode frames are acked at once but kept, because a
    /// static widget may never send another frame).
    pub acked: bool,
}

/// Newest not-yet-uploaded frame per widget.
#[derive(Default)]
pub struct Frames {
    pending: HashMap<u32, PendingFrame>,
}

impl Frames {
    /// Queues `f`; returns the id of the frame it replaced if that one still needs an ack.
    pub fn on_frame(&mut self, mut f: PendingFrame) -> Option<u64> {
        f.acked = false;
        self.pending.insert(f.widget, f).filter(|old| !old.acked).map(|old| old.id)
    }

    /// Marks every pending frame acked and returns the ids that weren't yet.
    pub fn ack_pending(&mut self) -> Vec<u64> {
        self.pending
            .values_mut()
            .filter(|f| !f.acked)
            .map(|f| {
                f.acked = true;
                f.id
            })
            .collect()
    }

    pub fn take(&mut self) -> Vec<PendingFrame> {
        self.pending.drain().map(|(_, f)| f).collect()
    }
}

/// Frame ids waiting for the GPU submission of a given swapchain image to complete.
#[derive(Default)]
pub struct AckQueue {
    by_frame: HashMap<usize, Vec<u64>>,
}

impl AckQueue {
    pub fn attach(&mut self, frame: usize, ids: Vec<u64>) {
        self.by_frame.entry(frame).or_default().extend(ids);
    }

    pub fn complete(&mut self, frame: usize) -> Vec<u64> {
        self.by_frame.remove(&frame).unwrap_or_default()
    }
}

/// A host shared-memory slot mapped into this process. `generation` changes every
/// time the host (re-)registers the slot. Mappings are never unmapped: the GPU may still read an
/// old one, and re-registration only happens on widget resize or host restart.
#[derive(Debug, Clone, Copy)]
pub struct SlotMap {
    pub addr: usize,
    pub len: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub drm_fourcc: u32,
    pub generation: u64,
}

const PAGE: usize = 4096;

#[derive(Default)]
struct Shared {
    frames: Frames,
    slots: HashMap<(u32, u32), SlotMap>,
    next_generation: u64,
    outgoing: Option<Sender<Msg>>,
    swapchain: Option<(u32, u32)>,
    /// Changes on every connect and disconnect, so the compositor can drop stale widgets.
    connection: u64,
    /// Latest host-set position and visibility per widget, not yet applied.
    layouts: HashMap<u32, (i32, i32, bool)>,
}

/// Edit mode as last set by the host.
static EDIT: AtomicBool = AtomicBool::new(false);

/// Raw-input state from the layer's own XInput2 connection.
#[derive(Default)]
struct Raw {
    hotkey: HotkeyState,
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
    /// Edit-mode cursor, swapchain px.
    cursor: (f32, f32),
}

static RAW: LazyLock<Mutex<Raw>> = LazyLock::new(Default::default);

const KEY_LSHIFT: u32 = 50;
const KEY_RSHIFT: u32 = 62;

pub fn cursor() -> (f32, f32) {
    RAW.lock().unwrap().cursor
}

fn send_input(ev: InputEvent) {
    if let Some(tx) = &SHARED.lock().unwrap().outgoing {
        let _ = tx.send(Msg::Input(ev));
    }
}

fn swapchain_extent() -> (u32, u32) {
    SHARED.lock().unwrap().swapchain.unwrap_or((1920, 1080))
}

/// Candidate H1 + C2: Ctrl+Tab toggles edit mode; in edit mode every raw key, button and
/// motion goes to the host (the game doesn't see them: `capture_wine_x11` filters them out).
fn on_raw(ev: RawInput) {
    let mut raw = RAW.lock().unwrap();
    match ev {
        RawInput::Key { code, down } => {
            if down {
                raw.keys.insert(code);
            } else {
                raw.keys.remove(&code);
            }
            if raw.hotkey.key(code, down) {
                let on = !edit_mode();
                eprintln!("[underwolf-spike] Ctrl+Tab: requesting edit mode {}", if on { "on" } else { "off" });
                drop(raw);
                request_edit(on);
                return;
            }
            if edit_mode() {
                let shift = raw.keys.contains(&KEY_LSHIFT) || raw.keys.contains(&KEY_RSHIFT);
                let text = if down { key_text(code, shift).map(String::from) } else { None };
                drop(raw);
                send_input(InputEvent::Key { code, down, text });
            }
        }
        RawInput::Button { button, down } => {
            if down {
                raw.buttons.insert(button);
            } else {
                raw.buttons.remove(&button);
            }
            if !edit_mode() {
                return;
            }
            drop(raw);
            match button {
                1..=3 => send_input(InputEvent::MouseButton { button: (button - 1) as u8, down }),
                4 if down => send_input(InputEvent::Wheel { dx: 0.0, dy: -100.0 }),
                5 if down => send_input(InputEvent::Wheel { dx: 0.0, dy: 100.0 }),
                _ => {}
            }
        }
        RawInput::Motion { dx, dy } => {
            if !edit_mode() {
                return;
            }
            let extent = swapchain_extent();
            raw.cursor = crate::overlay::move_cursor(raw.cursor, (dx as f32, dy as f32), extent);
            let (x, y) = raw.cursor;
            drop(raw);
            send_input(InputEvent::MouseMove { x, y });
        }
    }
}

/// The host switched edit mode: start or stop keeping input away from the game.
fn on_edit_mode(on: bool) {
    let was = EDIT.swap(on, Ordering::Relaxed);
    if on == was {
        return;
    }
    let mut raw = RAW.lock().unwrap();
    if on {
        let (w, h) = swapchain_extent();
        raw.cursor = (w as f32 / 2.0, h as f32 / 2.0);
        capture_wine_x11::start(raw.keys.iter().copied(), raw.buttons.iter().copied());
        let (x, y) = raw.cursor;
        drop(raw);
        send_input(InputEvent::MouseMove { x, y });
    } else {
        capture_wine_x11::stop();
    }
}

pub fn edit_mode() -> bool {
    EDIT.load(Ordering::Relaxed)
}

/// The hotkey asks the host to enter or leave edit mode; the host decides and echoes it back.
pub fn request_edit(on: bool) {
    if let Some(tx) = &SHARED.lock().unwrap().outgoing {
        let _ = tx.send(Msg::EditMode { on });
    }
}

pub fn take_layouts() -> HashMap<u32, (i32, i32, bool)> {
    std::mem::take(&mut SHARED.lock().unwrap().layouts)
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(Default::default);

pub fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = std::thread::Builder::new().name("uw-spike-client".into()).spawn(connection_loop);
        // Spike 1: H1 hotkey + C2 capture.
        match capture_wine_x11::install() {
            Ok(n) => eprintln!("[underwolf-spike] input filter installed in winex11.so ({n} imports)"),
            Err(e) => eprintln!("[underwolf-spike] input filter not installed: {e}"),
        }
        let _ = hotkey_xi2::spawn_raw(on_raw);
    });
}

pub fn take_frames() -> Vec<PendingFrame> {
    SHARED.lock().unwrap().frames.take()
}

/// Acks every pending frame but keeps it for a later upload.
pub fn ack_pending() {
    let mut shared = SHARED.lock().unwrap();
    let ids = shared.frames.ack_pending();
    if let Some(tx) = &shared.outgoing {
        for id in ids {
            let _ = tx.send(Msg::Ack { id });
        }
    }
}

pub fn connection() -> u64 {
    SHARED.lock().unwrap().connection
}

pub fn slot(widget: u32, slot: u32) -> Option<SlotMap> {
    SHARED.lock().unwrap().slots.get(&(widget, slot)).copied()
}

pub fn ack(ids: impl IntoIterator<Item = u64>) {
    let shared = SHARED.lock().unwrap();
    if let Some(tx) = &shared.outgoing {
        for id in ids {
            let _ = tx.send(Msg::Ack { id });
        }
    }
}

pub fn swapchain(width: u32, height: u32) {
    let mut shared = SHARED.lock().unwrap();
    shared.swapchain = Some((width, height));
    if let Some(tx) = &shared.outgoing {
        let _ = tx.send(Msg::Swapchain { width, height });
    }
}

fn map_slot(fd: &OwnedFd, stride: u32, height: u32) -> Option<(usize, usize)> {
    let len = (stride as usize * height as usize).div_ceil(PAGE) * PAGE;
    // Mapped read-write because host-pointer import pins the pages, which the NVIDIA driver
    // refuses for read-only mappings (ERROR_OUT_OF_DEVICE_MEMORY). The layer never writes here.
    // SAFETY: mapping a shared-memory fd; the result is checked.
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0)
    };
    (p != libc::MAP_FAILED).then_some((p as usize, len))
}

fn connection_loop() {
    let path = spike_dir().join("overlay.sock");
    loop {
        let Ok(stream) = UnixStream::connect(&path) else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let Ok(writer) = stream.try_clone() else { continue };
        let (tx, rx) = channel::<Msg>();
        let _ = tx.send(Msg::Hello { protocol: PROTOCOL, pid: std::process::id() });
        {
            let mut shared = SHARED.lock().unwrap();
            if let Some((width, height)) = shared.swapchain {
                let _ = tx.send(Msg::Swapchain { width, height });
            }
            shared.outgoing = Some(tx);
            shared.connection += 1;
        }
        let sender = std::thread::Builder::new().name("uw-spike-send".into()).spawn(move || {
            for msg in rx {
                if send(&writer, &msg, &[]).is_err() {
                    break;
                }
            }
        });

        while let Ok((msg, mut fds)) = recv(&stream) {
            match msg {
                Msg::Slot { widget, slot, width, height, stride, drm_fourcc } => {
                    let Some(fd) = fds.pop() else { continue };
                    let Some((addr, len)) = map_slot(&fd, stride, height) else { continue };
                    let mut shared = SHARED.lock().unwrap();
                    shared.next_generation += 1;
                    let generation = shared.next_generation;
                    shared
                        .slots
                        .insert((widget, slot), SlotMap { addr, len, width, height, stride, drm_fourcc, generation });
                }
                Msg::FrameShm { id, widget, slot, x, y } => {
                    let mut shared = SHARED.lock().unwrap();
                    let stale = if shared.slots.contains_key(&(widget, slot)) {
                        shared.frames.on_frame(PendingFrame { id, widget, slot, x, y, acked: false })
                    } else {
                        Some(id)
                    };
                    if let (Some(stale), Some(tx)) = (stale, &shared.outgoing) {
                        let _ = tx.send(Msg::Ack { id: stale });
                    }
                }
                // GPU shared textures aren't used on this path (plan Amendment A).
                Msg::Frame { id, .. } => ack([id]),
                Msg::EditMode { on } => on_edit_mode(on),
                Msg::Layout { widget, x, y, visible } => {
                    SHARED.lock().unwrap().layouts.insert(widget, (x, y, visible));
                }
                _ => {}
            }
        }

        let mut shared = SHARED.lock().unwrap();
        shared.outgoing = None;
        shared.connection += 1;
        shared.layouts.clear();
        drop(shared);
        on_edit_mode(false);
        let mut shared = SHARED.lock().unwrap();
        shared.frames.take();
        shared.slots.clear();
        drop(shared);
        if let Ok(h) = sender {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_frame_supersedes_pending_one() {
        let mut s = Frames::default();
        assert_eq!(s.on_frame(PendingFrame { id: 1, widget: 0, slot: 0, x: 10, y: 20, acked: false }), None);
        assert_eq!(s.on_frame(PendingFrame { id: 2, widget: 1, slot: 0, x: 0, y: 0, acked: false }), None);
        // Widget 0's frame 1 was never drawn; it is returned so it can be acked right away.
        assert_eq!(s.on_frame(PendingFrame { id: 3, widget: 0, slot: 1, x: 11, y: 21, acked: false }), Some(1));
        let mut taken = s.take();
        taken.sort_by_key(|f| f.id);
        assert_eq!(taken.iter().map(|f| f.id).collect::<Vec<_>>(), vec![2, 3]);
        assert!(s.take().is_empty());
    }

    #[test]
    fn acked_frame_is_kept_until_taken() {
        let mut s = Frames::default();
        s.on_frame(PendingFrame { id: 1, widget: 0, slot: 0, x: 0, y: 0, acked: false });
        assert_eq!(s.ack_pending(), vec![1]);
        assert!(s.ack_pending().is_empty(), "no double ack");
        let taken = s.take();
        assert_eq!(taken.len(), 1);
        assert!(taken[0].acked);
    }

    #[test]
    fn superseding_an_acked_frame_does_not_ack_again() {
        let mut s = Frames::default();
        s.on_frame(PendingFrame { id: 1, widget: 0, slot: 0, x: 0, y: 0, acked: false });
        s.ack_pending();
        assert_eq!(s.on_frame(PendingFrame { id: 2, widget: 0, slot: 1, x: 0, y: 0, acked: false }), None);
        assert_eq!(s.take().iter().map(|f| (f.id, f.acked)).collect::<Vec<_>>(), vec![(2, false)]);
    }

    #[test]
    fn ack_queue_releases_ids_per_frame_slot() {
        let mut q = AckQueue::default();
        q.attach(0, vec![5, 6]);
        q.attach(1, vec![7]);
        q.attach(0, vec![8]);
        assert_eq!(q.complete(0), vec![5, 6, 8]);
        assert!(q.complete(0).is_empty());
        assert_eq!(q.complete(1), vec![7]);
    }
}
