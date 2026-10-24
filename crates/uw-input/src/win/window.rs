use super::*;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::*,
    System::Threading::GetCurrentThreadId,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
use windows_sys::core::w;
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static ADAPTERS: OnceLock<bool> = OnceLock::new();
pub(super) fn message() -> Result<u32, InstallError> {
    // SAFETY: static NUL-terminated registration name.
    let msg = unsafe { RegisterWindowMessageW(w!("Underwolf.Input.Owner.v1")) };
    if msg == 0 { Err(InstallError("RegisterWindowMessageW failed".into())) } else { Ok(msg) }
}
pub(super) fn wake(s: &Shared) {
    // SAFETY: private wake; owned queue data isn't exposed as a foreign pointer.
    unsafe {
        PostMessageW(s.hwnd as HWND, s.message, 0, 0);
    }
}
pub(super) fn install(s: &Shared) -> Result<(), InstallError> {
    let mut module = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets resident callback address as an address, not a string.
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            owner_hook as *const u16,
            &mut module,
        )
    } == 0
    {
        return Err(InstallError("pin input module failed".into()));
    }
    // SAFETY: same-process UI thread and resident ABI-correct callback.
    if unsafe { SetWindowsHookExW(WH_GETMESSAGE, Some(owner_hook), std::ptr::null_mut(), s.thread) }.is_null() {
        return Err(InstallError("install owner hook failed".into()));
    }
    wake(s);
    Ok(())
}
fn initialize(s: &Shared) {
    let ready = *ADAPTERS.get_or_init(|| {
        let mut targets = keyboard::targets();
        targets.extend(raw::targets());
        targets.extend(registration::targets());
        targets.extend(cursor::targets());
        let pairs = targets.iter().map(|t| (t.address, t.detour)).collect::<Vec<_>>();
        if let Err(e) = crate::hooks::HookSet::install(&mut hooks::Backend(&targets), &pairs) {
            eprintln!("input hook setup: {}", e.message);
            return false;
        }
        // SAFETY: called on target thread; resident callback functions remain loaded for session.
        let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD, Some(key_hook), std::ptr::null_mut(), s.thread) };
        // SAFETY: same-thread mouse hook with resident callback.
        let mouse = unsafe { SetWindowsHookExW(WH_MOUSE, Some(mouse_hook), std::ptr::null_mut(), s.thread) };
        if keyboard.is_null() || mouse.is_null() {
            // SAFETY: remove only successfully installed thread hooks owned here.
            unsafe {
                if !keyboard.is_null() {
                    UnhookWindowsHookEx(keyboard);
                }
                if !mouse.is_null() {
                    UnhookWindowsHookEx(mouse);
                }
            }
            return false;
        }
        true
    });
    s.installed.store(ready, Ordering::Release);
    if let Some(e) = s.event(PolicyEvent::Installed(ready)) {
        s.effects(e)
    }
    // SAFETY: no pointer arguments for foreground query.
    if let Some(e) = s.event(PolicyEvent::Foreground(unsafe { GetForegroundWindow() } as usize == s.hwnd)) {
        s.effects(e)
    }
    // Initialize held-state conservatively: an injected module may arrive mid-press.
    for vk in 1..256 {
        if keyboard::real_async(vk) & i16::MIN != 0
            && let Some(e) = s.event(PolicyEvent::Input(InputRecord::key(vk, 0, true, false, false, 0)))
        {
            s.effects(e)
        }
    }
    s.report(if ready { InputReason::Ready } else { InputReason::SetupFailed }, None);
}
unsafe extern "system" fn owner_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let run = std::panic::catch_unwind(|| {
        if code < 0 || w != PM_REMOVE as usize {
            return;
        }
        let Some(s) = SHARED.get() else { return };
        // SAFETY: WH_GETMESSAGE provides MSG for nonnegative hook codes.
        let msg = unsafe { &mut *(l as *mut MSG) };
        // VK_PACKET uses the scan word for UTF-16 on Wine. Its hook lParam
        // cannot reliably distinguish up/down; the queued MSG can. Translate
        // that exact message once, then hide it from the application pump.
        if msg.hwnd as usize == s.hwnd
            && msg.wParam == VK_PACKET as usize
            && matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
            && s.capture()
        {
            // SAFETY: WH_GETMESSAGE supplied a valid mutable MSG for PM_REMOVE.
            bypass(|| unsafe {
                TranslateMessage(msg);
            });
            msg.message = WM_NULL;
            msg.wParam = 0;
            msg.lParam = 0;
            return;
        }
        if msg.hwnd as usize != s.hwnd || msg.message != s.message {
            return;
        }
        if ORIGINAL.load(Ordering::Acquire) == 0 {
            // SAFETY: owner UI thread replaces this window's procedure once.
            let old = unsafe { SetWindowLongPtrW(msg.hwnd, GWLP_WNDPROC, wndproc as *const () as isize) };
            if old != 0 {
                ORIGINAL.store(old as usize, Ordering::Release);
                initialize(s)
            }
        }
        service(s);
    });
    if run.is_err()
        && let Some(s) = SHARED.get()
    {
        s.revoke(InputReason::Unavailable)
    }
    // SAFETY: forwards all untouched hook arguments, including negative codes.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}
fn service(s: &Shared) {
    // SAFETY: no arguments.
    if unsafe { GetCurrentThreadId() } != s.thread {
        return;
    }
    s.retry_restore();
    let reason = s.release.swap(0, Ordering::AcqRel);
    if reason > 0
        && let Ok(r) = InputReason::from_code((reason - 1) as u8)
        && let Some(e) = s.event(PolicyEvent::Failure(r))
    {
        s.effects(e)
    }
    loop {
        let next = s.owner_events.try_lock().ok().and_then(|mut q| q.pop_front());
        let Some((epoch, event)) = next else { break };
        if epoch.is_none_or(|epoch| epoch == s.epoch.load(Ordering::Acquire))
            && let Some(e) = s.event(event)
        {
            s.effects(e);
            s.report(InputReason::Ready, None)
        }
    }
    while let Some(r) = s.requests.pop() {
        if let Some(e) = s.event(PolicyEvent::Request(r)) {
            s.effects(e)
        }
    }
}
fn original(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let p = ORIGINAL.load(Ordering::Acquire);
    // SAFETY: original procedure is published on the owner thread before subsequent dispatches.
    unsafe {
        if p == 0 {
            DefWindowProcW(hwnd, msg, w, l)
        } else {
            CallWindowProcW(std::mem::transmute::<usize, WNDPROC>(p), hwnd, msg, w, l)
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let run = std::panic::catch_unwind(|| {
        let s = SHARED.get()?;
        if msg == s.message {
            service(s);
            return Some(0);
        }
        if msg == WM_SETFOCUS || msg == WM_KILLFOCUS || msg == WM_DESTROY {
            let event = match msg {
                WM_SETFOCUS => PolicyEvent::Foreground(true),
                WM_DESTROY => PolicyEvent::Destroyed,
                _ => PolicyEvent::Foreground(false),
            };
            if let Some(e) = s.event(event) {
                s.effects(e);
                s.report(
                    match msg {
                        WM_SETFOCUS => InputReason::Ready,
                        WM_DESTROY => InputReason::WindowDestroyed,
                        _ => InputReason::FocusLost,
                    },
                    None,
                )
            }
        }
        if matches!(msg, WM_CHAR | WM_SYSCHAR)
            && let Some(e) = s.event(PolicyEvent::Char(w as u16))
        {
            let pass = e.pass;
            s.effects(e);
            if !pass {
                return Some(0);
            }
        }
        if s.capture()
            && matches!(
                msg,
                WM_KEYDOWN
                    | WM_KEYUP
                    | WM_SYSKEYDOWN
                    | WM_SYSKEYUP
                    | WM_MOUSEMOVE
                    | WM_LBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_RBUTTONDOWN
                    | WM_RBUTTONUP
                    | WM_MBUTTONDOWN
                    | WM_MBUTTONUP
                    | WM_XBUTTONDOWN
                    | WM_XBUTTONUP
                    | WM_MOUSEWHEEL
                    | WM_MOUSEHWHEEL
            )
        {
            return Some(0);
        }
        None
    });
    match run {
        Ok(Some(result)) => result,
        Ok(None) => original(hwnd, msg, w, l),
        Err(_) => {
            if let Some(s) = SHARED.get() {
                s.revoke(InputReason::Unavailable)
            }
            original(hwnd, msg, w, l)
        }
    }
}
unsafe extern "system" fn key_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let run = std::panic::catch_unwind(|| {
        if code != HC_ACTION as i32 || bypassed() {
            return false;
        }
        // The owner WH_GETMESSAGE hook translates and consumes Unicode packets
        // using their actual message kind, rather than overlapping scan/flag bits.
        if w == VK_PACKET as usize {
            return false;
        }
        let Some(s) = SHARED.get() else { return false };
        // SAFETY: keyboard focus query takes no arguments.
        if unsafe { GetFocus() } as usize != s.hwnd {
            return false;
        }
        service(s);
        let scan = ((l as u64 >> 16) & 255) as i32;
        let extended = l as u64 & (1 << 24) != 0;
        let down = l as u64 & (1 << 31) == 0;
        let repeat = l as u64 & (1 << 30) != 0;
        let vk = match w as i32 {
            17 => {
                if extended {
                    163
                } else {
                    162
                }
            }
            16 => {
                if scan == 0x36 {
                    161
                } else {
                    160
                }
            }
            18 => {
                if extended {
                    165
                } else {
                    164
                }
            }
            v => v,
        };
        let modifiers = s.policy.try_lock().map(|p| p.modifiers()).unwrap_or(0);
        let Some(e) = s.event(PolicyEvent::Input(InputRecord::key(vk, scan, down, extended, repeat, modifiers))) else {
            return false;
        };
        let pass = e.pass;
        let translate = down && e.host.iter().flatten().any(|r| r.kind == 1);
        s.effects(e);
        if translate {
            let msg = MSG {
                hwnd: s.hwnd as HWND,
                message: if l as u64 & (1 << 29) != 0 { WM_SYSKEYDOWN } else { WM_KEYDOWN },
                wParam: w,
                lParam: l,
                ..Default::default()
            };
            // SAFETY: reconstructed key message uses the hook's exact scan/context flags; bypass preserves real key state.
            bypass(|| unsafe {
                TranslateMessage(&msg);
            });
        }
        !pass
    });
    match run {
        Ok(true) => 1,
        other => {
            if other.is_err()
                && let Some(s) = SHARED.get()
            {
                s.revoke(InputReason::Unavailable)
            }
            // SAFETY: forward unhandled hook arguments and negative codes unchanged.
            unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
        }
    }
}
unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    let run = std::panic::catch_unwind(|| {
        if code != HC_ACTION as i32 || bypassed() {
            return false;
        }
        let Some(s) = SHARED.get() else { return false };
        // SAFETY: nonnegative mouse hook supplies a MOUSEHOOKSTRUCTEX.
        let m = unsafe { &*(l as *const MOUSEHOOKSTRUCTEX) };
        if m.Base.hwnd as usize != s.hwnd {
            return false;
        }
        service(s);
        let mut point = POINT { x: m.Base.pt.x, y: m.Base.pt.y };
        // SAFETY: selected valid window and writable point.
        unsafe {
            ScreenToClient(s.hwnd as HWND, &mut point);
        }
        let modifiers = s.policy.try_lock().map(|p| p.modifiers()).unwrap_or(0);
        let r = match w as u32 {
            WM_MOUSEMOVE => InputRecord::motion(point.x, point.y, modifiers),
            WM_LBUTTONDOWN | WM_LBUTTONUP => {
                InputRecord::button(0, w as u32 == WM_LBUTTONDOWN, point.x, point.y, modifiers)
            }
            WM_MBUTTONDOWN | WM_MBUTTONUP => {
                InputRecord::button(1, w as u32 == WM_MBUTTONDOWN, point.x, point.y, modifiers)
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP => {
                InputRecord::button(2, w as u32 == WM_RBUTTONDOWN, point.x, point.y, modifiers)
            }
            WM_XBUTTONDOWN | WM_XBUTTONUP => InputRecord::button(
                2 + ((m.mouseData >> 16) & 0xffff) as i32,
                w as u32 == WM_XBUTTONDOWN,
                point.x,
                point.y,
                modifiers,
            ),
            WM_MOUSEWHEEL => InputRecord::wheel(0, (m.mouseData >> 16) as i16 as i32, point.x, point.y, modifiers),
            WM_MOUSEHWHEEL => InputRecord::wheel((m.mouseData >> 16) as i16 as i32, 0, point.x, point.y, modifiers),
            _ => return false,
        };
        let Some(e) = s.event(PolicyEvent::Input(r)) else { return false };
        let pass = e.pass;
        s.effects(e);
        !pass
    });
    match run {
        Ok(true) => 1,
        other => {
            if other.is_err()
                && let Some(s) = SHARED.get()
            {
                s.revoke(InputReason::Unavailable)
            }
            // SAFETY: forward unhandled hook arguments, including all negative codes.
            unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
        }
    }
}
pub(super) fn release_held(s: &Shared, releases: &[Option<InputRecord>; 261]) -> bool {
    // SAFETY: foreground query has no arguments; never inject after focus changed.
    if unsafe { GetForegroundWindow() } as usize != s.hwnd {
        return false;
    }
    if !raw::expect_releases(releases) {
        return false;
    }
    let mut inputs = [INPUT::default(); 261];
    let mut count = 0;
    for r in releases.iter().flatten() {
        if r.kind == 1 {
            let l = (1isize << 31) | (1 << 30) | ((r.b as isize) << 16) | if r.flags & 2 != 0 { 1 << 24 } else { 0 };
            original(s.hwnd as HWND, WM_KEYUP, r.a as usize, l);
            inputs[count] = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: r.a as u16,
                        wScan: r.b as u16,
                        dwFlags: KEYEVENTF_KEYUP | if r.flags & 2 != 0 { KEYEVENTF_EXTENDEDKEY } else { 0 },
                        time: 0,
                        dwExtraInfo: crate::raw::RELEASE_TAG as usize,
                    },
                },
            };
        } else if r.kind == 4 {
            let (flags, data) = match r.a {
                0 => (MOUSEEVENTF_LEFTUP, 0),
                1 => (MOUSEEVENTF_MIDDLEUP, 0),
                2 => (MOUSEEVENTF_RIGHTUP, 0),
                3 => (MOUSEEVENTF_XUP, 1),
                4 => (MOUSEEVENTF_XUP, 2),
                _ => continue,
            };
            inputs[count] = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: data,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: crate::raw::RELEASE_TAG as usize,
                    },
                },
            };
        } else {
            continue;
        }
        count += 1;
    }
    // SAFETY: initialized INPUT prefix, correct native ABI size, foreground verified.
    count == 0
        || unsafe { SendInput(count as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) } == count as u32
}

pub(super) fn observe(s: &Shared, record: InputRecord) -> bool {
    // SAFETY: current thread query has no arguments.
    if unsafe { GetCurrentThreadId() } == s.thread {
        if let Some(effects) = s.event(PolicyEvent::Observed(record)) {
            let blocked = !effects.pass;
            s.effects(effects);
            return blocked;
        }
    } else {
        s.submit(PolicyEvent::Observed(record));
    }
    s.capture() || (record.kind == 1 && s.blocks_key(record.a))
}
