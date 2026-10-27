//! Owner-thread text and fallback for native paths proven to hide thread/raw input.
use super::*;
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    System::{LibraryLoader::*, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
static WANTED: AtomicBool = AtomicBool::new(false);
static KEYBOARD: AtomicUsize = AtomicUsize::new(0);
static MOUSE: AtomicUsize = AtomicUsize::new(0);
static OWNER: AtomicU32 = AtomicU32::new(0);
static REFRESH: AtomicBool = AtomicBool::new(false);
thread_local! { static TEXT_KEYS: Cell<[u8; 256]> = const { Cell::new([0; 256]) }; }
thread_local! { static CALLBACK: Cell<bool> = const { Cell::new(false) }; }
fn callback<T>(operation: impl FnOnce() -> T) -> T {
    struct Guard(bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            CALLBACK.with(|c| c.set(self.0));
        }
    }
    let _guard = Guard(CALLBACK.with(|c| c.replace(true)));
    operation()
}
pub(super) fn request() {
    let new = !WANTED.swap(true, Ordering::AcqRel);
    if let Some(s) = SHARED.get() {
        if new && s.routing_mode.load(Ordering::Acquire) {
            s.revoke(InputReason::Unavailable);
        }
        window::wake(s);
    }
}
pub(super) fn active() -> bool {
    KEYBOARD.load(Ordering::Acquire) != 0 && MOUSE.load(Ordering::Acquire) != 0
}
pub(super) fn ready() -> bool {
    !WANTED.load(Ordering::Acquire) || (active() && !REFRESH.load(Ordering::Acquire))
}
pub(super) fn invalidate() {
    if WANTED.load(Ordering::Acquire) {
        REFRESH.store(true, Ordering::Release);
    }
}
pub(super) fn retire() {
    for slot in [&KEYBOARD, &MOUSE] {
        let hook = slot.swap(0, Ordering::AcqRel);
        if hook != 0 {
            // SAFETY: only this module's published resident callbacks are retired.
            unsafe {
                UnhookWindowsHookEx(hook as HHOOK);
            }
        }
    }
    OWNER.store(0, Ordering::Release);
}
pub(super) fn ensure(s: &Shared) {
    if CALLBACK.with(Cell::get) {
        return;
    }
    if !WANTED.load(Ordering::Acquire) || !s.routing_mode.load(Ordering::Acquire) {
        return;
    }
    if active() && OWNER.load(Ordering::Acquire) == s.thread() && !REFRESH.swap(false, Ordering::AcqRel) {
        return;
    }
    retire();
    let mut module = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS pins this callback's actual containing module.
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            keyboard as *const u16,
            &mut module,
        )
    } == 0
    {
        s.revoke(InputReason::SetupFailed);
        return;
    }
    // SAFETY: callbacks stay resident, and low-level hooks execute on this installing UI owner thread.
    let key = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0) };
    // SAFETY: same owner-thread and pinned-module lifetime for the mouse callback.
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0) };
    KEYBOARD.store(key as usize, Ordering::Release);
    MOUSE.store(mouse as usize, Ordering::Release);
    if key.is_null() || mouse.is_null() {
        retire();
        s.revoke(InputReason::SetupFailed);
        return;
    }
    OWNER.store(s.thread(), Ordering::Release);
    REFRESH.store(false, Ordering::Release);
    seed(s);
    reset_text();
    native::request_coverage();
}
pub(super) fn seed(s: &Shared) {
    let mut state = [0u8; 256];
    // SAFETY: writable complete keyboard state; bypass obtains the native toggle state.
    bypass(|| unsafe {
        GetKeyboardState(state.as_mut_ptr());
    });
    for (vk, byte) in state.iter_mut().enumerate() {
        *byte = (*byte & 1) | if keyboard::real_async(vk as i32) & i16::MIN != 0 { 0x80 } else { 0 };
    }
    TEXT_KEYS.with(|keys| keys.set(state));
    if let Ok(mut p) = s.policy.try_lock() {
        // Queue-local key-up messages can be delivered to another foreground
        // window. The current native snapshot replaces those stale held bits.
        p.resample_keyboard(std::array::from_fn(|vk| state[vk] & 0x80 != 0));
        let physical = p.physical_keys();
        for (vk, held) in physical.iter().enumerate().skip(1) {
            if !held {
                p.router.physical_release(InputRecord::key(vk as i32, 0, false, false, false, 0));
            }
        }
        p.hotkeys.reseed(&physical);
        s.publish_suppression(&p);
    } else {
        s.revoke(InputReason::Unavailable);
    }
}
fn background_release(record: InputRecord) {
    let Some(s) = SHARED.get() else {
        return;
    };
    // SAFETY: retired owner callbacks must never mutate a replacement window's session.
    if unsafe { GetCurrentThreadId() } != s.thread() || OWNER.load(Ordering::Acquire) != s.thread() {
        return;
    }
    // SAFETY: this path only accounts for releases outside the selected foreground window.
    if unsafe { GetForegroundWindow() } as usize == s.hwnd() {
        return;
    }
    if s.permit.load(Ordering::Acquire) {
        s.revoke(InputReason::FocusLost);
        window::service(s);
    }
    if let Ok(mut p) = s.policy.try_lock() {
        let tracked = match record.kind {
            1 => p.physical_keys()[record.a as usize],
            4 => p.physical_buttons()[record.a as usize],
            _ => false,
        };
        if tracked {
            p.physical_release(record);
            p.router.physical_release(record);
            let physical = p.physical_keys();
            p.hotkeys.observe(record, &physical);
            s.publish_suppression(&p);
        }
    }
}
fn scope() -> Option<&'static Shared> {
    let s = SHARED.get()?;
    // SAFETY: owner identity and foreground queries take no pointer arguments.
    if unsafe { GetCurrentThreadId() } != s.thread() || OWNER.load(Ordering::Acquire) != s.thread() {
        return None;
    }
    // SAFETY: foreground query is scalar; desktop events always pass through untouched.
    if unsafe { GetForegroundWindow() } as usize != s.hwnd() {
        if s.permit.load(Ordering::Acquire) {
            s.revoke(InputReason::FocusLost);
        }
        return None;
    }
    Some(s)
}
fn key_state(vk: usize, down: bool) {
    TEXT_KEYS.with(|keys| {
        let mut state = keys.get();
        if down && state[vk] & 0x80 == 0 && matches!(vk, 20 | 144 | 145) {
            state[vk] ^= 1;
        }
        state[vk] = (state[vk] & 1) | if down { 0x80 } else { 0 };
        for (generic, left, right) in [(16, 160, 161), (17, 162, 163), (18, 164, 165)] {
            state[generic] = (state[left] | state[right]) & 0x80;
        }
        keys.set(state);
    });
}
/// Canonical raw keys also need owner-thread character translation when no legacy message exists.
pub(super) fn raw_key(s: &Shared, record: InputRecord, translate: bool) {
    if record.kind != 1 || !(1..=255).contains(&record.a) {
        return;
    }
    key_state(record.a as usize, record.flags & 1 != 0);
    if translate && record.a != VK_PACKET as i32 {
        text(s, record);
    }
}
pub(super) fn reset_text() {
    // Drain pending native dead-key state on the owner thread without delivering it to another widget.
    let mut output = [0u16; 8];
    let state = [0u8; 256];
    // SAFETY: complete state and writable UTF-16 buffer, using this UI owner's current layout.
    unsafe {
        ToUnicodeEx(32, 57, state.as_ptr(), output.as_mut_ptr(), output.len() as i32, 0, GetKeyboardLayout(0));
    }
}
fn text(s: &Shared, record: InputRecord) {
    let state = TEXT_KEYS.with(Cell::get);
    let mut output = [0u16; 8];
    // SAFETY: bounded UTF-16 output and a complete physical state, on the actual owner thread.
    let count = unsafe {
        ToUnicodeEx(
            record.a as u32,
            record.b as u32,
            state.as_ptr(),
            output.as_mut_ptr(),
            output.len() as i32,
            0,
            GetKeyboardLayout(0),
        )
    };
    if count > 0 {
        for unit in output.into_iter().take((count as usize).min(8)) {
            s.routed_char(unit as u32, true);
        }
    }
}
unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let consumed = callback(|| {
        boundary(false, || {
            if code != HC_ACTION as i32 || bypassed() {
                return false;
            }
            // SAFETY: nonnegative low-level keyboard hook supplies this native structure.
            let key = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
            if key.dwExtraInfo as u32 == crate::raw::RELEASE_TAG {
                return false;
            }
            // Wine's Unicode packet callbacks can omit LLKHF_UP; wParam retains
            // the exact WM_KEYDOWN/UP message, as the independent probe verifies.
            let down = matches!(w as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let vk = match key.vkCode {
                16 => {
                    if key.scanCode == 0x36 {
                        161
                    } else {
                        160
                    }
                }
                17 => {
                    if key.flags & LLKHF_EXTENDED != 0 {
                        163
                    } else {
                        162
                    }
                }
                18 => {
                    if key.flags & LLKHF_EXTENDED != 0 {
                        165
                    } else {
                        164
                    }
                }
                vk => vk,
            };
            let Some(s) = scope() else {
                if !down && vk > 0 && vk < 256 {
                    background_release(InputRecord::key(
                        vk as i32,
                        (key.scanCode & 255) as i32,
                        false,
                        key.flags & LLKHF_EXTENDED != 0,
                        false,
                        0,
                    ));
                }
                return false;
            };
            window::service(s);
            if key.vkCode == VK_PACKET as u32 {
                if down {
                    return !s.routed_char(key.scanCode & 0xffff, true);
                }
                return s.capture_domain(false);
            }
            if vk == 0 || vk >= 256 {
                return false;
            }
            key_state(vk as usize, down);
            let record = InputRecord::key(
                vk as i32,
                (key.scanCode & 255) as i32,
                down,
                key.flags & LLKHF_EXTENDED != 0,
                false,
                0,
            );
            let Some((effects, hotkey_consumed)) = s.ingress_lowlevel(record) else {
                return false;
            };
            // Preserve the desktop modifier state used by the system's foreground-switch shortcuts.
            // Thread/raw/polling adapters still filter these edges from the captured game.
            let pass = effects.pass || matches!(vk, 16..=18 | 160..=165) && !hotkey_consumed;
            let system_escape = down
                && !hotkey_consumed
                && TEXT_KEYS.with(|keys| {
                    let state = keys.get();
                    let alt = state[18] & 0x80 != 0;
                    matches!(vk, 91 | 92) || alt && matches!(vk, 9 | 27 | 115) || vk == 27 && state[17] & 0x80 != 0
                });
            let translate = effects.host[0].is_some();
            s.effects(effects);
            if system_escape {
                s.revoke(InputReason::FocusLost);
                return false;
            }
            if translate {
                text(s, record);
            }
            if !down {
                directinput::synchronize(false);
            }
            !pass
        })
    });
    if consumed {
        1
    } else {
        // SAFETY: all desktop/negative/unconsumed events retain their original hook arguments.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
    }
}
unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let consumed = callback(|| {
        boundary(false, || {
            if code != HC_ACTION as i32 || bypassed() {
                return false;
            }
            // SAFETY: nonnegative low-level mouse hook supplies this native structure.
            let mouse = unsafe { &*(l as *const MSLLHOOKSTRUCT) };
            if mouse.dwExtraInfo as u32 == crate::raw::RELEASE_TAG {
                return false;
            }
            let Some(s) = scope() else {
                let button = match w as u32 {
                    WM_LBUTTONUP => Some(0),
                    WM_MBUTTONUP => Some(1),
                    WM_RBUTTONUP => Some(2),
                    WM_XBUTTONUP => Some(2 + (mouse.mouseData >> 16) as i32),
                    _ => None,
                };
                if let Some(button) = button {
                    background_release(InputRecord::button(button, false, 0, 0, 0));
                }
                return false;
            };
            window::service(s);
            let mut point = POINT { x: mouse.pt.x, y: mouse.pt.y };
            // SAFETY: process-owned foreground window, writable point.
            unsafe {
                ScreenToClient(s.hwnd() as HWND, &mut point);
            }
            let record = match w as u32 {
                WM_MOUSEMOVE => InputRecord::motion(point.x, point.y, 0),
                WM_LBUTTONDOWN | WM_LBUTTONUP => {
                    InputRecord::button(0, w as u32 == WM_LBUTTONDOWN, point.x, point.y, 0)
                }
                WM_MBUTTONDOWN | WM_MBUTTONUP => {
                    InputRecord::button(1, w as u32 == WM_MBUTTONDOWN, point.x, point.y, 0)
                }
                WM_RBUTTONDOWN | WM_RBUTTONUP => {
                    InputRecord::button(2, w as u32 == WM_RBUTTONDOWN, point.x, point.y, 0)
                }
                WM_XBUTTONDOWN | WM_XBUTTONUP => InputRecord::button(
                    2 + (mouse.mouseData >> 16) as i32,
                    w as u32 == WM_XBUTTONDOWN,
                    point.x,
                    point.y,
                    0,
                ),
                WM_MOUSEWHEEL => InputRecord::wheel(0, (mouse.mouseData >> 16) as i16 as i32, point.x, point.y, 0),
                WM_MOUSEHWHEEL => InputRecord::wheel((mouse.mouseData >> 16) as i16 as i32, 0, point.x, point.y, 0),
                _ => return false,
            };
            // Foreground alone does not identify the target of the first desktop click.
            // SAFETY: native physical coordinates and scalar root-window queries.
            let outside = unsafe { GetAncestor(WindowFromPhysicalPoint(mouse.pt), GA_ROOT) as usize != s.hwnd() };
            let paired = if let Ok(p) = s.policy.try_lock() {
                p.router.pointer_captured()
                    || record.kind == 4
                        && record.flags & 1 == 0
                        && (p.physical_buttons()[record.a as usize]
                            || p.key_suppressed([1, 4, 2, 5, 6][record.a as usize]))
            } else {
                s.revoke(InputReason::Unavailable);
                return false;
            };
            if outside && !paired && record.kind == 4 {
                if record.flags & 1 != 0 {
                    // Leave native devices isolated until the OS has processed this activation.
                    s.revoke(InputReason::FocusLost);
                }
                return false;
            }
            if outside && !paired && record.kind == 5 {
                return false;
            }
            let Some((effects, _)) = s.ingress_lowlevel(record) else {
                return false;
            };
            let pass = effects.pass || outside && !paired;
            s.effects(effects);
            if matches!(w as u32, WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP | WM_XBUTTONUP) {
                directinput::synchronize(false);
            }
            if !pass && s.capture_domain(true) {
                // SAFETY: the consumed physical move still advances the overlay's real desktop pointer.
                bypass(|| unsafe {
                    SetCursorPos(mouse.pt.x, mouse.pt.y);
                });
            }
            !pass
        })
    });
    if consumed {
        1
    } else {
        // SAFETY: desktop/negative/unconsumed events pass through unchanged.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
    }
}
