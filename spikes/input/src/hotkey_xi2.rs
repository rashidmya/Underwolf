//! Candidate H1: detect the edit-mode hotkey (Ctrl+Tab) from inside the game process by
//! reading XInput2 *raw* key events on a separate X connection. Read-only: nothing is grabbed
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

/// Watches the X server for Ctrl+Tab on a background thread, calling `on_hotkey` each time.
/// Reconnects every second if the connection fails.
pub fn spawn(on_hotkey: impl Fn() + Send + 'static) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("uw-spike-hotkey".into()).spawn(move || {
        loop {
            if let Err(e) = watch(&on_hotkey) {
                eprintln!("[underwolf-spike] hotkey (XI2) watcher: {e}; retrying");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    })
}

fn watch(on_hotkey: &dyn Fn()) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    conn.xinput_xi_query_version(2, 2)?.reply()?;
    let masks = [xinput::EventMask {
        deviceid: 1, // XIAllMasterDevices
        mask: vec![xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE],
    }];
    conn.xinput_xi_select_events(root, &masks)?.check()?;
    let mut state = HotkeyState::default();
    loop {
        let (code, down) = match conn.wait_for_event()? {
            Event::XinputRawKeyPress(e) => (e.detail, true),
            Event::XinputRawKeyRelease(e) => (e.detail, false),
            _ => continue,
        };
        if state.key(code, down) {
            on_hotkey();
        }
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
    fn either_ctrl_works() {
        let mut h = HotkeyState::default();
        h.key(KEY_RCTRL, true);
        assert!(h.key(KEY_TAB, true));
    }
}
