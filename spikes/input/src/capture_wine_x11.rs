//! Candidate C2: while edit mode is on, keep mouse and keyboard events away from the game by
//! filtering them where Wine's X11 driver (`winex11.so`) reads X events, so the game keeps focus
//! and never sees overlay input. Only the input path is touched; game memory and logic are not.
//! The overlay gets its input from the layer's own XInput2 raw-event connection instead.

use std::collections::HashSet;

/// `sizeof(XEvent)` on x86_64 (a union padded to 24 longs).
pub const XEVENT_SIZE: usize = 192;

pub const KEY_PRESS: i32 = 2;
pub const KEY_RELEASE: i32 = 3;
pub const BUTTON_PRESS: i32 = 4;
pub const BUTTON_RELEASE: i32 = 5;
pub const MOTION_NOTIFY: i32 = 6;
pub const GENERIC_EVENT: i32 = 35;

pub const XI_KEY_PRESS: i32 = 2;
pub const XI_KEY_RELEASE: i32 = 3;
pub const XI_BUTTON_PRESS: i32 = 4;
pub const XI_BUTTON_RELEASE: i32 = 5;
pub const XI_MOTION: i32 = 6;
pub const XI_RAW_KEY_PRESS: i32 = 13;
pub const XI_RAW_KEY_RELEASE: i32 = 14;
pub const XI_RAW_BUTTON_PRESS: i32 = 15;
pub const XI_RAW_BUTTON_RELEASE: i32 = 16;
pub const XI_RAW_MOTION: i32 = 17;

// Field offsets (x86_64 Xlib): XKeyEvent.keycode / XButtonEvent.button, and
// XGenericEventCookie.extension / .evtype.
const DETAIL_OFFSET: usize = 84;
const EXTENSION_OFFSET: usize = 32;
const EVTYPE_OFFSET: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Swallow,
}

fn i32_at(e: &[u8], at: usize) -> i32 {
    i32::from_ne_bytes(e[at..at + 4].try_into().expect("4 bytes"))
}

/// Decides, per X event, whether the game may see it.
pub struct Filter {
    pub(crate) xi_opcode: i32,
    capturing: bool,
    /// Keys/buttons already down when capture started: their releases still go to the game,
    /// so nothing stays stuck (e.g. Ctrl = crouch from the Ctrl+Tab that opened edit mode).
    held_keys: HashSet<u32>,
    held_buttons: HashSet<u32>,
}

impl Filter {
    pub fn new(xi_opcode: i32) -> Filter {
        Filter { xi_opcode, capturing: false, held_keys: HashSet::new(), held_buttons: HashSet::new() }
    }

    pub fn start(&mut self, held_keys: impl IntoIterator<Item = u32>, held_buttons: impl IntoIterator<Item = u32>) {
        self.capturing = true;
        self.held_keys = held_keys.into_iter().collect();
        self.held_buttons = held_buttons.into_iter().collect();
    }

    pub fn stop(&mut self) {
        self.capturing = false;
        self.held_keys.clear();
        self.held_buttons.clear();
    }

    pub fn capturing(&self) -> bool {
        self.capturing
    }

    pub fn verdict(&mut self, e: &[u8]) -> Verdict {
        if !self.capturing || e.len() < XEVENT_SIZE {
            return Verdict::Pass;
        }
        let detail = || u32::from_ne_bytes(e[DETAIL_OFFSET..DETAIL_OFFSET + 4].try_into().expect("4 bytes"));
        match i32_at(e, 0) {
            KEY_PRESS | BUTTON_PRESS | MOTION_NOTIFY => Verdict::Swallow,
            KEY_RELEASE => {
                if self.held_keys.remove(&detail()) { Verdict::Pass } else { Verdict::Swallow }
            }
            BUTTON_RELEASE => {
                if self.held_buttons.remove(&detail()) { Verdict::Pass } else { Verdict::Swallow }
            }
            GENERIC_EVENT if i32_at(e, EXTENSION_OFFSET) == self.xi_opcode => match i32_at(e, EVTYPE_OFFSET) {
                // Which button an XI2 release is for is only in the cookie data; while any
                // button held at entry is still down, let XI2 button releases through.
                XI_BUTTON_RELEASE | XI_RAW_BUTTON_RELEASE if !self.held_buttons.is_empty() => Verdict::Pass,
                XI_KEY_PRESS | XI_KEY_RELEASE | XI_BUTTON_PRESS | XI_BUTTON_RELEASE | XI_MOTION
                | XI_RAW_KEY_PRESS | XI_RAW_KEY_RELEASE | XI_RAW_BUTTON_PRESS | XI_RAW_BUTTON_RELEASE
                | XI_RAW_MOTION => Verdict::Swallow,
                _ => Verdict::Pass,
            },
            _ => Verdict::Pass,
        }
    }
}

// ---- Installation into Wine's X11 driver ------------------------------------------------------

use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::got::{Module, find_slots};

static FILTER: LazyLock<Mutex<Filter>> = LazyLock::new(|| Mutex::new(Filter::new(-1)));
static ORIG_CHECK_IF_EVENT: AtomicUsize = AtomicUsize::new(0);
static ORIG_CHECK_TYPED_WINDOW_EVENT: AtomicUsize = AtomicUsize::new(0);
static X_QUERY_EXTENSION: AtomicUsize = AtomicUsize::new(0);

type CheckIfEvent = unsafe extern "C" fn(*mut c_void, *mut u8, *const c_void, *mut c_char) -> c_int;
type CheckTypedWindowEvent = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut u8) -> c_int;
type QueryExtension = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int, *mut c_int, *mut c_int) -> c_int;

/// Puts the filter in front of `winex11.so`'s `XCheckIfEvent` and `XCheckTypedWindowEvent`.
/// Returns how many import slots were redirected.
pub fn install() -> Result<usize, String> {
    unsafe {
        let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        if lib.is_null() {
            return Err("libX11.so.6 is not loaded".into());
        }
        let sym = |name: &std::ffi::CStr| libc::dlsym(lib, name.as_ptr()) as usize;
        let (check_if, check_typed, query) =
            (sym(c"XCheckIfEvent"), sym(c"XCheckTypedWindowEvent"), sym(c"XQueryExtension"));
        if check_if == 0 || check_typed == 0 || query == 0 {
            return Err("libX11 symbols missing".into());
        }
        // The real functions come from libX11 itself, not from the slots: an unresolved lazy
        // slot points at a PLT stub that would overwrite our redirection when called.
        ORIG_CHECK_IF_EVENT.store(check_if, Ordering::SeqCst);
        ORIG_CHECK_TYPED_WINDOW_EVENT.store(check_typed, Ordering::SeqCst);
        X_QUERY_EXTENSION.store(query, Ordering::SeqCst);
        let slots = find_slots(Module::Suffix("/winex11.so"), &["XCheckIfEvent", "XCheckTypedWindowEvent"]);
        if slots.is_empty() {
            return Err("winex11.so (or its XCheckIfEvent import) not found".into());
        }
        for s in &slots {
            let hook = match s.symbol.as_str() {
                "XCheckIfEvent" => check_if_event as *const () as usize,
                _ => check_typed_window_event as *const () as usize,
            };
            s.patch(hook);
        }
        Ok(slots.len())
    }
}

/// Starts keeping input away from the game. Releases of `held_keys` / `held_buttons` (down
/// right now) still reach it.
pub fn start(held_keys: impl IntoIterator<Item = u32>, held_buttons: impl IntoIterator<Item = u32>) {
    FILTER.lock().unwrap().start(held_keys, held_buttons);
}

pub fn stop() {
    FILTER.lock().unwrap().stop();
}

unsafe fn verdict(display: *mut c_void, ev: *mut u8) -> Verdict {
    let mut f = FILTER.lock().unwrap();
    if !f.capturing() {
        return Verdict::Pass;
    }
    if f.xi_opcode < 0 {
        let query: QueryExtension = unsafe { std::mem::transmute(X_QUERY_EXTENSION.load(Ordering::SeqCst)) };
        let (mut op, mut ev_base, mut err_base) = (0, 0, 0);
        if unsafe { query(display, c"XInputExtension".as_ptr(), &mut op, &mut ev_base, &mut err_base) } != 0 {
            f.xi_opcode = op;
        }
    }
    f.verdict(unsafe { std::slice::from_raw_parts(ev, XEVENT_SIZE) })
}

unsafe extern "C" fn check_if_event(display: *mut c_void, ev: *mut u8, pred: *const c_void, arg: *mut c_char) -> c_int {
    let orig: CheckIfEvent = unsafe { std::mem::transmute(ORIG_CHECK_IF_EVENT.load(Ordering::Relaxed)) };
    loop {
        let r = unsafe { orig(display, ev, pred, arg) };
        if r == 0 || unsafe { verdict(display, ev) } == Verdict::Pass {
            return r;
        }
    }
}

unsafe extern "C" fn check_typed_window_event(display: *mut c_void, w: c_ulong, ty: c_int, ev: *mut u8) -> c_int {
    let orig: CheckTypedWindowEvent =
        unsafe { std::mem::transmute(ORIG_CHECK_TYPED_WINDOW_EVENT.load(Ordering::Relaxed)) };
    loop {
        let r = unsafe { orig(display, w, ty, ev) };
        if r == 0 || unsafe { verdict(display, ev) } == Verdict::Pass {
            return r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XI: i32 = 131;

    #[test]
    fn install_outside_wine_fails_cleanly() {
        assert!(install().is_err());
    }

    fn core(ty: i32, detail: u32) -> [u8; XEVENT_SIZE] {
        let mut e = [0u8; XEVENT_SIZE];
        e[0..4].copy_from_slice(&ty.to_ne_bytes());
        e[84..88].copy_from_slice(&detail.to_ne_bytes());
        e
    }

    fn generic(extension: i32, evtype: i32) -> [u8; XEVENT_SIZE] {
        let mut e = [0u8; XEVENT_SIZE];
        e[0..4].copy_from_slice(&GENERIC_EVENT.to_ne_bytes());
        e[32..36].copy_from_slice(&extension.to_ne_bytes());
        e[36..40].copy_from_slice(&evtype.to_ne_bytes());
        e
    }

    #[test]
    fn passes_everything_when_not_capturing() {
        let mut f = Filter::new(XI);
        assert_eq!(f.verdict(&core(KEY_PRESS, 38)), Verdict::Pass);
        assert_eq!(f.verdict(&core(MOTION_NOTIFY, 0)), Verdict::Pass);
    }

    #[test]
    fn swallows_input_while_capturing() {
        let mut f = Filter::new(XI);
        f.start([], []);
        for e in [core(KEY_PRESS, 38), core(KEY_RELEASE, 38), core(BUTTON_PRESS, 1), core(BUTTON_RELEASE, 1), core(MOTION_NOTIFY, 0)] {
            assert_eq!(f.verdict(&e), Verdict::Swallow);
        }
        for xi in [XI_KEY_PRESS, XI_MOTION, XI_RAW_MOTION, XI_RAW_BUTTON_PRESS, XI_RAW_KEY_PRESS] {
            assert_eq!(f.verdict(&generic(XI, xi)), Verdict::Swallow, "XI evtype {xi}");
        }
    }

    #[test]
    fn non_input_events_always_pass() {
        let mut f = Filter::new(XI);
        f.start([], []);
        assert_eq!(f.verdict(&core(9 /* FocusIn */, 0)), Verdict::Pass);
        assert_eq!(f.verdict(&core(22 /* ConfigureNotify */, 0)), Verdict::Pass);
        assert_eq!(f.verdict(&generic(77, XI_RAW_MOTION)), Verdict::Pass, "other extensions pass");
    }

    #[test]
    fn releases_of_keys_held_at_entry_reach_the_game_once() {
        let mut f = Filter::new(XI);
        f.start([37 /* LCtrl */, 23 /* Tab */], [3]);
        assert_eq!(f.verdict(&core(KEY_RELEASE, 23)), Verdict::Pass);
        assert_eq!(f.verdict(&core(KEY_RELEASE, 37)), Verdict::Pass);
        assert_eq!(f.verdict(&core(KEY_PRESS, 37)), Verdict::Swallow, "a new press is overlay input");
        assert_eq!(f.verdict(&core(KEY_RELEASE, 37)), Verdict::Swallow, "...and so is its release");
        assert_eq!(f.verdict(&core(BUTTON_RELEASE, 3)), Verdict::Pass);
        assert_eq!(f.verdict(&core(BUTTON_RELEASE, 3)), Verdict::Swallow);
    }

    #[test]
    fn stop_restores_pass_through() {
        let mut f = Filter::new(XI);
        f.start([], []);
        f.stop();
        assert_eq!(f.verdict(&core(KEY_PRESS, 38)), Verdict::Pass);
    }
}
