//! Candidate C3: a Win32-side input hook running inside the Deadlock process
//! under Proton.
//!
//! Deadlock is a Windows build; under Proton its input arrives through Wine's
//! input driver and is delivered as Win32 messages and raw input. This module
//! hooks that path **in-process** — it subclasses the game window's `WndProc`
//! and consumes raw mouse input — and routes every event through
//! [`crate::gate::InputGate`]. It touches only the input path: no game memory is
//! read or written and no game logic is hooked, matching the spec's rule for
//! candidates C2/C3.
//!
//! ## What is tested here vs. on hardware
//!
//! The parts that decide behavior are portable and unit-tested on any platform:
//! [`translate_message`] (Win32 message → [`RawInput`]) and [`handle_message`]
//! (the glue that turns a gate [`Step`](crate::gate::Step) into a swallow/pass
//! decision, synthetic key releases, and host forwarding).
//!
//! The actual hook installation ([`install`], the `WndProc`, raw-input
//! registration, `DllMain`) is `#[cfg(windows)]` FFI. It is compiled for the
//! Proton target (`x86_64-pc-windows-gnu`) and verified by the owner against the
//! P1–P6 matrix in the Hideout; it cannot run in CI on the Linux host.

use std::path::PathBuf;

use uw_spike_ipc::InputEvent;

use crate::gate::{GameAction, InputGate, RawInput, Step};

/// Win32 window-message constants this module reasons about.
pub mod wm {
    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_KEYUP: u32 = 0x0101;
    pub const WM_SYSKEYDOWN: u32 = 0x0104;
    pub const WM_SYSKEYUP: u32 = 0x0105;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    pub const WM_RBUTTONDOWN: u32 = 0x0204;
    pub const WM_RBUTTONUP: u32 = 0x0205;
    pub const WM_MBUTTONDOWN: u32 = 0x0207;
    pub const WM_MBUTTONUP: u32 = 0x0208;
    pub const WM_MOUSEWHEEL: u32 = 0x020A;
    pub const WM_MOUSEHWHEEL: u32 = 0x020E;
}

// Button numbering matches the host/ipc convention (`InputEvent::MouseButton`): 0 left, 1 middle,
// 2 right. The Electron host indexes `['left', 'middle', 'right']` by this.
pub const BTN_LEFT: u8 = 0;
pub const BTN_MIDDLE: u8 = 1;
pub const BTN_RIGHT: u8 = 2;

const WHEEL_DELTA: f32 = 120.0;

fn loword_signed(v: isize) -> i16 {
    (v & 0xFFFF) as u16 as i16
}
fn hiword_signed(v: isize) -> i16 {
    ((v >> 16) & 0xFFFF) as u16 as i16
}

/// Translate a Win32 window message into a [`RawInput`], or `None` for messages
/// the gate does not care about.
pub fn translate_message(msg: u32, wparam: usize, lparam: isize) -> Option<RawInput> {
    match msg {
        wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN => {
            Some(RawInput::Key { code: wparam as u32, down: true, text: None })
        }
        wm::WM_KEYUP | wm::WM_SYSKEYUP => {
            Some(RawInput::Key { code: wparam as u32, down: false, text: None })
        }
        wm::WM_LBUTTONDOWN => Some(RawInput::MouseButton { button: BTN_LEFT, down: true }),
        wm::WM_LBUTTONUP => Some(RawInput::MouseButton { button: BTN_LEFT, down: false }),
        wm::WM_RBUTTONDOWN => Some(RawInput::MouseButton { button: BTN_RIGHT, down: true }),
        wm::WM_RBUTTONUP => Some(RawInput::MouseButton { button: BTN_RIGHT, down: false }),
        wm::WM_MBUTTONDOWN => Some(RawInput::MouseButton { button: BTN_MIDDLE, down: true }),
        wm::WM_MBUTTONUP => Some(RawInput::MouseButton { button: BTN_MIDDLE, down: false }),
        wm::WM_MOUSEMOVE => Some(RawInput::MouseMove {
            x: loword_signed(lparam) as f32,
            y: hiword_signed(lparam) as f32,
        }),
        wm::WM_MOUSEWHEEL => Some(RawInput::Wheel {
            dx: 0.0,
            dy: hiword_signed(wparam as isize) as f32 / WHEEL_DELTA,
        }),
        wm::WM_MOUSEHWHEEL => Some(RawInput::Wheel {
            dx: hiword_signed(wparam as isize) as f32 / WHEEL_DELTA,
            dy: 0.0,
        }),
        _ => None,
    }
}

/// RAWINPUT layout on x86_64. `RAWINPUTHEADER` is 24 bytes; `RAWMOUSE` follows. Field offsets are
/// into the whole RAWINPUT buffer. (Source 2 tooling / `winuser.h`.)
mod ri {
    pub const HEADER_TYPE: usize = 0; // DWORD dwType
    pub const MOUSE_BUTTON_FLAGS: usize = 28; // USHORT usButtonFlags
    pub const MOUSE_BUTTON_DATA: usize = 30; // USHORT usButtonData (signed wheel delta)
    pub const MOUSE_RAW_BUTTONS: usize = 32; // ULONG  ulRawButtons
    pub const MOUSE_LAST_X: usize = 36; // LONG lLastX
    pub const MOUSE_LAST_Y: usize = 40; // LONG lLastY
    pub const EXTRA_INFO: usize = 44; // ULONG ulExtraInformation
    pub const MIN_LEN: usize = 44; // through lLastY

    pub const RIM_TYPEMOUSE: u32 = 0;

    pub const LEFT_DOWN: u16 = 0x0001;
    pub const LEFT_UP: u16 = 0x0002;
    pub const RIGHT_DOWN: u16 = 0x0004;
    pub const RIGHT_UP: u16 = 0x0008;
    pub const MIDDLE_DOWN: u16 = 0x0010;
    pub const MIDDLE_UP: u16 = 0x0020;
    pub const WHEEL: u16 = 0x0400;
    pub const HWHEEL: u16 = 0x0800;
}

fn is_mouse_record(buf: &[u8]) -> bool {
    buf.len() >= ri::MIN_LEN
        && u32::from_le_bytes(buf[ri::HEADER_TYPE..ri::HEADER_TYPE + 4].try_into().unwrap()) == ri::RIM_TYPEMOUSE
}

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}
fn read_i32(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

/// Parse a RAWINPUT mouse record into the raw events it represents, in a stable order: motion,
/// then each button transition, then the wheel. Returns empty for non-mouse or truncated records.
pub fn parse_raw_mouse(buf: &[u8]) -> Vec<RawInput> {
    if !is_mouse_record(buf) {
        return Vec::new();
    }
    let flags = read_u16(buf, ri::MOUSE_BUTTON_FLAGS);
    let data = read_u16(buf, ri::MOUSE_BUTTON_DATA) as i16;
    let dx = read_i32(buf, ri::MOUSE_LAST_X);
    let dy = read_i32(buf, ri::MOUSE_LAST_Y);

    let mut out = Vec::new();
    if dx != 0 || dy != 0 {
        out.push(RawInput::MouseMove { x: dx as f32, y: dy as f32 });
    }
    for (bit, button, down) in [
        (ri::LEFT_DOWN, BTN_LEFT, true),
        (ri::LEFT_UP, BTN_LEFT, false),
        (ri::RIGHT_DOWN, BTN_RIGHT, true),
        (ri::RIGHT_UP, BTN_RIGHT, false),
        (ri::MIDDLE_DOWN, BTN_MIDDLE, true),
        (ri::MIDDLE_UP, BTN_MIDDLE, false),
    ] {
        if flags & bit != 0 {
            out.push(RawInput::MouseButton { button, down });
        }
    }
    if flags & ri::WHEEL != 0 {
        out.push(RawInput::Wheel { dx: 0.0, dy: data as f32 / WHEEL_DELTA });
    }
    if flags & ri::HWHEEL != 0 {
        out.push(RawInput::Wheel { dx: data as f32 / WHEEL_DELTA, dy: 0.0 });
    }
    out
}

/// The RAWMOUSE `ulExtraInformation` tag of a mouse record, or `None` for non-mouse/short records.
/// The Win32 hook writes a known tag on its own synthetic releases and reads it here to pass them
/// through to the game instead of neutralizing them.
pub fn raw_mouse_extra_info(buf: &[u8]) -> Option<u32> {
    (buf.len() >= ri::EXTRA_INFO + 4 && is_mouse_record(buf))
        .then(|| u32::from_le_bytes(buf[ri::EXTRA_INFO..ri::EXTRA_INFO + 4].try_into().unwrap()))
}

/// Minimum RAWINPUT length the mouse parser reads (through `lLastY`).
pub fn ri_min_len() -> usize {
    ri::MIN_LEN
}

/// Zero the motion, button and wheel fields of a RAWINPUT mouse record in place, so a game that
/// reads it next sees no mouse-look and no clicks. No-op for non-mouse or truncated records.
pub fn neutralize_raw_mouse(buf: &mut [u8]) {
    if !is_mouse_record(buf) {
        return;
    }
    // usButtonFlags, usButtonData, ulRawButtons, lLastX, lLastY — everything a reader acts on.
    buf[ri::MOUSE_BUTTON_FLAGS..ri::MOUSE_LAST_Y + 4].fill(0);
    debug_assert_eq!(ri::MOUSE_BUTTON_FLAGS, 28);
    debug_assert_eq!(ri::MOUSE_RAW_BUTTONS, 32);
}

/// Where the overlay host listens. The home directory is visible inside the
/// Proton container, so a path under `~/.cache` is reachable from the hook.
pub fn host_socket_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".cache/underwolf-spike/overlay.sock")
}

/// What the `WndProc` must do with one message after the gate has seen it.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageOutcome {
    /// If true, do not forward the current message to the game's `WndProc`.
    pub swallow: bool,
    /// Synthetic releases to inject into the game before returning (so that keys
    /// held when edit mode opened do not stick down).
    pub synth: Vec<GameAction>,
}

/// Where host-bound events go. The `cfg(windows)` hook implements this over the
/// IPC socket; tests implement it with a recording fake.
pub trait HostSink {
    fn input(&mut self, event: &InputEvent);
    fn edit_mode_changed(&mut self, on: bool);
}

/// Turn a gate [`Step`] into the `WndProc`'s marching orders and forward the
/// host-bound half of it.
pub fn apply_step(step: &Step, host: &mut impl HostSink) -> MessageOutcome {
    for event in &step.to_host {
        host.input(event);
    }
    if let Some(on) = step.edit_mode_changed {
        host.edit_mode_changed(on);
    }

    // The current message reaches the game only if the gate asked to pass it
    // through; anything else (edit-mode swallow, a release-only transition) means
    // swallow it. Synthetic releases are injected regardless.
    let mut swallow = true;
    let mut synth = Vec::new();
    for action in &step.to_game {
        match action {
            GameAction::Pass(_) => swallow = false,
            other => synth.push(other.clone()),
        }
    }
    MessageOutcome { swallow, synth }
}

/// Full per-record raw-mouse path: run every event in a RAWINPUT mouse record through the gate,
/// forward the host-bound half, and — while edit mode swallows it — neutralize the record in place
/// so the game's `GetRawInputData` returns no motion or clicks (criterion P2). Outside edit mode
/// the record is left intact for the game and nothing is forwarded.
pub fn handle_raw_mouse(buf: &mut [u8], gate: &mut InputGate, host: &mut impl HostSink) {
    let mut swallow = false;
    for input in parse_raw_mouse(buf) {
        let step = gate.step(input);
        swallow |= apply_step(&step, host).swallow;
    }
    if swallow {
        neutralize_raw_mouse(buf);
    }
}

/// The byte ranges of the first `count` records in a `GetRawInputBuffer` batch. Each record's
/// `dwSize` gives its length and the next starts 8-byte aligned (`NEXTRAWINPUTBLOCK` on x86_64;
/// the caller's buffer is aligned). Stops early rather than run past the buffer.
pub fn raw_buffer_records(buf: &[u8], count: u32) -> Vec<std::ops::Range<usize>> {
    const DW_SIZE: usize = 4;
    let mut out = Vec::new();
    let mut off = 0usize;
    for _ in 0..count {
        let Some(size) = buf.get(off + DW_SIZE..off + DW_SIZE + 4) else { break };
        let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
        if size == 0 || off + size > buf.len() {
            break;
        }
        out.push(off..off + size);
        off = (off + size).next_multiple_of(8);
    }
    out
}

/// One RAWINPUT record from either `GetRawInputData` or a `GetRawInputBuffer` batch: our own
/// tagged synthetic release passes through to the game with the tag stripped; any other mouse
/// record goes through [`handle_raw_mouse`].
pub fn handle_raw_record(rec: &mut [u8], synth_tag: u32, gate: &mut InputGate, host: &mut impl HostSink) {
    if raw_mouse_extra_info(rec) == Some(synth_tag) {
        rec[ri::EXTRA_INFO..ri::EXTRA_INFO + 4].fill(0);
    } else {
        handle_raw_mouse(rec, gate, host);
    }
}

/// [`handle_raw_record`] for every record of a `GetRawInputBuffer` batch of `count` records.
pub fn handle_raw_buffer(buf: &mut [u8], count: u32, synth_tag: u32, gate: &mut InputGate, host: &mut impl HostSink) {
    for range in raw_buffer_records(buf, count) {
        handle_raw_record(&mut buf[range], synth_tag, gate, host);
    }
}

/// Full per-message path: translate, run the gate, forward to the host, and tell
/// the caller what to do with the game.
pub fn handle_message(
    msg: u32,
    wparam: usize,
    lparam: isize,
    gate: &mut InputGate,
    host: &mut impl HostSink,
) -> MessageOutcome {
    match translate_message(msg, wparam, lparam) {
        Some(input) => {
            let step = gate.step(input);
            apply_step(&step, host)
        }
        // A message the gate does not model always reaches the game untouched.
        None => MessageOutcome { swallow: false, synth: Vec::new() },
    }
}

#[cfg(windows)]
mod plugin;
#[cfg(windows)]
mod win32;
