//! Candidate H1: detect the edit-mode hotkey (Ctrl+Tab) from inside the game process by
//! reading XInput2 *raw* key events on a separate X connection. The same raw stream (keys,
//! buttons, motion) feeds the overlay during edit mode under candidate C2. Read-only: nothing is grabbed
//! or consumed, the game still gets every key. Under a Wayland session the game is an
//! XWayland client, and XWayland delivers raw events while an XWayland window has focus.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _};

/// XKB keycodes (evdev + 8).
pub const KEY_LCTRL: u32 = 37;
pub const KEY_RCTRL: u32 = 105;
pub const KEY_TAB: u32 = 23;

/// Turns a raw key stream into hotkey activations.
#[derive(Default)]
pub struct HotkeyState {
    lctrl: bool,
    rctrl: bool,
    tab: bool,
}

impl HotkeyState {
    /// Feeds one key event; returns true when Ctrl+Tab was just pressed.
    pub fn key(&mut self, code: u32, down: bool) -> bool {
        match code {
            KEY_LCTRL => self.lctrl = down,
            KEY_RCTRL => self.rctrl = down,
            KEY_TAB => {
                let pressed = down && !self.tab;
                self.tab = down;
                return pressed && (self.lctrl || self.rctrl);
            }
            _ => {}
        }
        false
    }
}

/// One raw input event (XInput2 raw events are read-only copies; nothing is consumed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawInput {
    /// XKB keycode.
    Key { code: u32, down: bool },
    /// X button: 1 left, 2 middle, 3 right, 4/5 wheel up/down.
    Button { button: u32, down: bool },
    /// Pointer motion (accelerated valuators), pixels.
    Motion { dx: f64, dy: f64 },
}

/// Pointer delta from a raw motion event: valuators 0 and 1 are x and y. `values` holds one
/// 32.32 fixed-point value per set mask bit, in bit order.
pub fn motion_delta(mask: &[u32], values: &[(i32, u32)]) -> (f64, f64) {
    let (mut dx, mut dy) = (0.0, 0.0);
    let mut next = values.iter();
    for (word, bits) in mask.iter().enumerate() {
        for bit in 0..32 {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let Some(&(int, frac)) = next.next() else { return (dx, dy) };
            let v = int as f64 + frac as f64 / 4_294_967_296.0;
            match word * 32 + bit {
                0 => dx = v,
                1 => dy = v,
                _ => {}
            }
        }
    }
    (dx, dy)
}

/// Text for a keycode on a US layout (enough for the spike's typing test).
pub fn key_text(code: u32, shift: bool) -> Option<char> {
    const DIGITS: &str = "1234567890-=";
    const ROW_Q: &str = "qwertyuiop[]";
    const ROW_A: &str = "asdfghjkl;'";
    const ROW_Z: &str = "zxcvbnm,./";
    let c = match code {
        10..=21 => DIGITS.chars().nth((code - 10) as usize),
        24..=35 => ROW_Q.chars().nth((code - 24) as usize),
        38..=48 => ROW_A.chars().nth((code - 38) as usize),
        52..=61 => ROW_Z.chars().nth((code - 52) as usize),
        65 => Some(' '),
        _ => None,
    }?;
    Some(if shift { c.to_ascii_uppercase() } else { c })
}

/// Watches the X server for Ctrl+Tab on a background thread, calling `on_hotkey` each time.
pub fn spawn(on_hotkey: impl Fn() + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    let mut state = HotkeyState::default();
    spawn_raw(move |ev| {
        if let RawInput::Key { code, down } = ev
            && state.key(code, down)
        {
            on_hotkey();
        }
    })
}

/// Streams raw keyboard, button and motion events to `handler` on a background thread.
/// Reconnects every second if the connection fails.
pub fn spawn_raw(mut handler: impl FnMut(RawInput) + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("uw-spike-rawinput".into()).spawn(move || {
        loop {
            if let Err(e) = watch(&mut handler) {
                eprintln!("[underwolf-spike] raw input (XI2) watcher: {e}; retrying");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    })
}

fn watch(handler: &mut dyn FnMut(RawInput)) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    conn.xinput_xi_query_version(2, 2)?.reply()?;
    let masks = [xinput::EventMask {
        deviceid: 1, // XIAllMasterDevices
        mask: vec![
            xinput::XIEventMask::RAW_KEY_PRESS
                | xinput::XIEventMask::RAW_KEY_RELEASE
                | xinput::XIEventMask::RAW_BUTTON_PRESS
                | xinput::XIEventMask::RAW_BUTTON_RELEASE
                | xinput::XIEventMask::RAW_MOTION,
        ],
    }];
    conn.xinput_xi_select_events(root, &masks)?.check()?;
    loop {
        let ev = match conn.wait_for_event()? {
            Event::XinputRawKeyPress(e) => RawInput::Key { code: e.detail, down: true },
            Event::XinputRawKeyRelease(e) => RawInput::Key { code: e.detail, down: false },
            Event::XinputRawButtonPress(e) => RawInput::Button { button: e.detail, down: true },
            Event::XinputRawButtonRelease(e) => RawInput::Button { button: e.detail, down: false },
            Event::XinputRawMotion(e) => {
                let vals: Vec<(i32, u32)> = e.axisvalues.iter().map(|v| (v.integral, v.frac)).collect();
                let (dx, dy) = motion_delta(&e.valuator_mask, &vals);
                RawInput::Motion { dx, dy }
            }
            _ => continue,
        };
        handler(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_tab_toggles_once_per_press() {
        let mut h = HotkeyState::default();
        assert!(!h.key(KEY_LCTRL, true));
        assert!(h.key(KEY_TAB, true));
        assert!(!h.key(KEY_TAB, true), "auto-repeat must not toggle again");
        assert!(!h.key(KEY_TAB, false));
        assert!(h.key(KEY_TAB, true), "second press while Ctrl held");
        assert!(!h.key(KEY_TAB, false));
        assert!(!h.key(KEY_LCTRL, false));
    }

    #[test]
    fn tab_alone_or_after_ctrl_release_does_nothing() {
        let mut h = HotkeyState::default();
        assert!(!h.key(KEY_TAB, true));
        assert!(!h.key(KEY_TAB, false));
        h.key(KEY_RCTRL, true);
        h.key(KEY_RCTRL, false);
        assert!(!h.key(KEY_TAB, true));
    }

    #[test]
    fn raw_motion_decodes_first_two_valuators() {
        // Valuators 0 (x) and 1 (y) set; x = 2.5, y = -1.25 as 32.32 fixed point.
        let mask = [0b11u32];
        let vals = [(2, 1u32 << 31), (-2, 3u32 << 30)];
        assert_eq!(motion_delta(&mask, &vals), (2.5, -1.25));
        // Only y moved.
        assert_eq!(motion_delta(&[0b10], &[(3, 0)]), (0.0, 3.0));
        assert_eq!(motion_delta(&[0], &[]), (0.0, 0.0));
    }

    #[test]
    fn us_keymap_text() {
        assert_eq!(key_text(38, false), Some('a'));
        assert_eq!(key_text(38, true), Some('A'));
        assert_eq!(key_text(10, false), Some('1'));
        assert_eq!(key_text(65, false), Some(' '));
        assert_eq!(key_text(9, false), None, "Escape has no text");
    }

    #[test]
    fn either_ctrl_works() {
        let mut h = HotkeyState::default();
        h.key(KEY_RCTRL, true);
        assert!(h.key(KEY_TAB, true));
    }
}
