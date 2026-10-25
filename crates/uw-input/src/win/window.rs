use super::*;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::*,
    System::Threading::GetCurrentThreadId,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
use windows_sys::core::w;
const ORIGINAL_PROPERTY: windows_sys::core::PCWSTR = w!("Underwolf.Input.Original.v1");
static OWNER_THREAD_HANDLE: AtomicUsize = AtomicUsize::new(0);
static OWNER_HOOK: AtomicUsize = AtomicUsize::new(0);
static KEY_HOOK: AtomicUsize = AtomicUsize::new(0);
static MOUSE_HOOK: AtomicUsize = AtomicUsize::new(0);
static ADAPTERS: OnceLock<bool> = OnceLock::new();
pub(super) fn message() -> Result<u32, InstallError> {
    // SAFETY: static NUL-terminated registration name.
    let msg = unsafe { RegisterWindowMessageW(w!("Underwolf.Input.Owner.v1")) };
    if msg == 0 { Err(InstallError("RegisterWindowMessageW failed".into())) } else { Ok(msg) }
}
pub(super) fn wake(s: &Shared) {
    // SAFETY: private wake; owned queue data isn't exposed as a foreign pointer.
    unsafe {
        if s.hwnd() == 0 {
            PostThreadMessageW(s.thread(), s.message, 0, 0);
        } else {
            PostMessageW(s.hwnd() as HWND, s.message, 0, 0);
        }
    }
}
pub(super) fn install(s: &Shared) -> Result<(), InstallError> {
    use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
    // SAFETY: validated same-process owner thread; retaining its handle prevents ID reuse ambiguity.
    let handle = unsafe { OpenThread(THREAD_SYNCHRONIZE, 0, s.thread()) };
    if handle.is_null() {
        return Err(InstallError("open owner thread failed".into()));
    }
    let previous = OWNER_THREAD_HANDLE.swap(handle as usize, Ordering::AcqRel);
    if previous != 0 {
        // SAFETY: closes only the previously retained thread-query handle.
        unsafe {
            CloseHandle(previous as *mut _);
        }
    }
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
    let hook = unsafe { SetWindowsHookExW(WH_GETMESSAGE, Some(owner_hook), std::ptr::null_mut(), s.thread()) };
    if hook.is_null() {
        return Err(InstallError("install owner hook failed".into()));
    }
    OWNER_HOOK.store(hook as usize, Ordering::Release);
    wake(s);
    Ok(())
}
fn install_thread_hooks(s: &Shared) -> bool {
    // SAFETY: target owner thread, with resident ABI-correct callbacks.
    let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD, Some(key_hook), std::ptr::null_mut(), s.thread()) };
    // SAFETY: target owner thread, with resident ABI-correct callback.
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE, Some(mouse_hook), std::ptr::null_mut(), s.thread()) };
    KEY_HOOK.store(keyboard as usize, Ordering::Release);
    MOUSE_HOOK.store(mouse as usize, Ordering::Release);
    if keyboard.is_null() || mouse.is_null() {
        retire_thread_hooks(false);
        return false;
    }
    true
}
fn retire_thread_hooks(owner: bool) {
    lowlevel::retire();
    for slot in [&KEY_HOOK, &MOUSE_HOOK].into_iter().chain(owner.then_some(&OWNER_HOOK)) {
        let hook = slot.swap(0, Ordering::AcqRel);
        if hook != 0 {
            // SAFETY: only our published hook handles are retired; callbacks remain pinned.
            unsafe {
                UnhookWindowsHookEx(hook as HHOOK);
            }
        }
    }
}
pub(super) fn rediscover(s: &Shared) {
    if s.hwnd() != 0 {
        return;
    }
    if s.restore_epoch.load(Ordering::Acquire) != 0 {
        let handle = OWNER_THREAD_HANDLE.load(Ordering::Acquire);
        if handle == 0
            // SAFETY: retained synchronization handle; zero timeout never blocks the watchdog.
            || unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(handle as *mut _, 0) } != 0
        {
            return;
        }
        if !cursor::abandon_queue() {
            return;
        }
    }
    unsafe extern "system" fn find(hwnd: HWND, data: LPARAM) -> i32 {
        let mut pid = 0;
        // SAFETY: enumerated window and local output pointer.
        unsafe {
            let thread = GetWindowThreadProcessId(hwnd, &mut pid);
            if thread != 0
                && pid == windows_sys::Win32::System::Threading::GetCurrentProcessId()
                && IsWindowVisible(hwnd) != 0
                && GetWindow(hwnd, GW_OWNER).is_null()
            {
                *(data as *mut (usize, u32)) = (hwnd as usize, thread);
                return 0;
            }
        }
        1
    }
    let mut found = (0usize, 0u32);
    // SAFETY: synchronous enumeration retains this writable local context.
    unsafe {
        EnumWindows(Some(find), &mut found as *mut (usize, u32) as LPARAM);
    }
    if found.0 == 0 {
        return;
    }
    retire_thread_hooks(true);
    s.thread.store(found.1, Ordering::Release);
    s.hwnd.store(found.0, Ordering::Release);
    if install(s).is_err() {
        s.hwnd.store(0, Ordering::Release);
    }
}
fn initialize(s: &Shared) {
    registration::observe_existing(s);
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
        true
    });
    let ready = ready && install_thread_hooks(s);
    s.installed.store(ready, Ordering::Release);
    if let Some(e) = s.event(PolicyEvent::Installed(ready)) {
        s.effects(e)
    }
    // SAFETY: no pointer arguments for foreground query.
    if let Some(e) = s.event(PolicyEvent::Foreground(unsafe { GetForegroundWindow() } as usize == s.hwnd())) {
        s.effects(e)
    }
    // Seed the same left/right-key and mouse-button representation used by hooks.
    let mut position = POINT::default();
    // SAFETY: writable point; original query bypasses game-visible virtualization.
    bypass(|| unsafe {
        GetCursorPos(&mut position);
        ScreenToClient(s.hwnd() as HWND, &mut position);
    });
    for vk in 1..256 {
        if keyboard::real_async(vk) & i16::MIN == 0 {
            continue;
        }
        let button = match vk {
            1 => Some(0),
            4 => Some(1),
            2 => Some(2),
            5 => Some(3),
            6 => Some(4),
            _ => None,
        };
        let record = if let Some(button) = button {
            InputRecord::button(button, true, position.x, position.y, 0)
        } else {
            // SAFETY: scalar mapping for this owner's current keyboard layout.
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
            InputRecord::key(vk, (scan & 255) as i32, true, scan & 0xff00 == 0xe000, false, 0)
        };
        if let Some(e) = s.event(PolicyEvent::Snapshot(record)) {
            s.effects(e);
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
        // SAFETY: owner-thread identity query has no arguments.
        if unsafe { GetCurrentThreadId() } != s.thread() {
            return;
        }
        // SAFETY: WH_GETMESSAGE provides MSG for nonnegative hook codes.
        let msg = unsafe { &mut *(l as *mut MSG) };
        if s.hwnd() == 0 && msg.message == s.message {
            service(s);
            return;
        }
        // VK_PACKET uses the scan word for UTF-16 on Wine. Its hook lParam
        // cannot reliably distinguish up/down; the queued MSG can. Translate
        // that exact message once, then hide it from the application pump.
        if msg.hwnd as usize == s.hwnd()
            && msg.wParam == VK_PACKET as usize
            && matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
            && s.capture_domain(false)
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
        if msg.hwnd as usize != s.hwnd() || msg.message != s.message {
            return;
        }
        // SAFETY: private per-window property stores its original procedure.
        if unsafe { GetPropW(msg.hwnd, ORIGINAL_PROPERTY) }.is_null() {
            // SAFETY: owner UI thread replaces this window's procedure once.
            let old = unsafe { SetWindowLongPtrW(msg.hwnd, GWLP_WNDPROC, wndproc as *const () as isize) };
            if old != 0 {
                // SAFETY: opaque procedure pointer belongs to this window, retained until NCDESTROY.
                if unsafe { SetPropW(msg.hwnd, ORIGINAL_PROPERTY, old as *mut _) } != 0 {
                    initialize(s)
                } else {
                    // SAFETY: rollback our just-installed owner-thread subclass.
                    unsafe {
                        SetWindowLongPtrW(msg.hwnd, GWLP_WNDPROC, old);
                    }
                    s.revoke(InputReason::SetupFailed);
                }
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
pub(super) fn service(s: &Shared) {
    // SAFETY: no arguments.
    if unsafe { GetCurrentThreadId() } != s.thread() {
        return;
    }
    lowlevel::ensure(s);
    s.retry_restore();
    // Hide is already an atomic revocation. Apply its queued state transaction before
    // invalidating the epoch, so its own edit-off request remains correlated.
    let hidden = InputReason::Hidden as usize + 1;
    let reason = if s.release.load(Ordering::Acquire) == hidden { 0 } else { s.release.swap(0, Ordering::AcqRel) };
    if reason > 0
        && let Ok(r) = InputReason::from_code((reason - 1) as u8)
        && let Some(e) = s.event(PolicyEvent::Failure(r))
    {
        s.effects(e)
    }
    loop {
        let next = s.owner_events.try_lock().ok().and_then(|mut q| q.pop_front());
        let Some((epoch, event)) = next else { break };
        let before = s.epoch.load(Ordering::Acquire);
        if epoch.is_none_or(|epoch| epoch == s.epoch.load(Ordering::Acquire))
            && let Some(e) = s.event(event)
        {
            if before != s.epoch.load(Ordering::Acquire)
                || !matches!(event, PolicyEvent::Healthy(_) | PolicyEvent::CaptureState { .. })
            {
                s.report(InputReason::Ready, None);
            }
            s.effects(e)
        }
    }
    while let Some(r) = s.requests.pop() {
        if let Some(e) = s.event(PolicyEvent::Request(r)) {
            s.effects(e)
        }
    }
    if s.release.compare_exchange(hidden, 0, Ordering::AcqRel, Ordering::Acquire).is_ok()
        && let Some(e) = s.event(PolicyEvent::Failure(InputReason::Hidden))
    {
        s.effects(e);
    }
    s.service_routing();
    directinput::synchronize(false);
}
fn original(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: opaque per-window procedure pointer is never dereferenced as data.
    let p = unsafe { GetPropW(hwnd, ORIGINAL_PROPERTY) } as usize;
    // SAFETY: original procedure is published on the owner thread before subsequent dispatches.
    directinput::game_callback(|| unsafe {
        if p == 0 {
            DefWindowProcW(hwnd, msg, w, l)
        } else {
            CallWindowProcW(std::mem::transmute::<usize, WNDPROC>(p), hwnd, msg, w, l)
        }
    })
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCDESTROY {
        let result = original(hwnd, msg, w, l);
        // SAFETY: final owner-thread dispatch; property is private and no longer needed.
        unsafe {
            RemovePropW(hwnd, ORIGINAL_PROPERTY);
        }
        if let Some(s) = SHARED.get()
            && s.hwnd() == hwnd as usize
        {
            s.installed.store(false, Ordering::Release);
            retire_thread_hooks(false);
            s.hwnd.store(0, Ordering::Release);
        }
        return result;
    }
    let run = std::panic::catch_unwind(|| {
        let s = SHARED.get()?;
        if s.hwnd() != hwnd as usize {
            return None;
        }
        if msg == s.message {
            service(s);
            return Some(0);
        }
        if msg == WM_SETFOCUS || msg == WM_KILLFOCUS || msg == WM_DESTROY {
            if msg == WM_SETFOCUS && lowlevel::active() {
                lowlevel::seed(s);
            }
            if msg == WM_DESTROY {
                registration::retire_window(hwnd as usize);
            }
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
        if s.routing_mode.load(Ordering::Acquire) && matches!(msg, WM_CHAR | WM_SYSCHAR | WM_UNICHAR) {
            if msg == WM_UNICHAR && w == 0xffff {
                return Some(1);
            }
            if !s.routed_char(w as u32, msg != WM_UNICHAR) {
                return Some(0);
            }
        } else if matches!(msg, WM_CHAR | WM_SYSCHAR)
            && let Some(e) = s.event(PolicyEvent::Char(w as u16))
        {
            let pass = e.pass;
            s.effects(e);
            if !pass {
                return Some(0);
            }
        }
        if (if s.routing_mode.load(Ordering::Acquire) {
            s.capture_domain(!matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP))
        } else {
            s.capture()
        }) && matches!(
            msg,
            WM_KEYDOWN
                | WM_KEYUP
                | WM_SYSKEYDOWN
                | WM_SYSKEYUP
                | WM_MOUSEMOVE
                | WM_LBUTTONDOWN
                | WM_LBUTTONDBLCLK
                | WM_LBUTTONUP
                | WM_RBUTTONDOWN
                | WM_RBUTTONDBLCLK
                | WM_RBUTTONUP
                | WM_MBUTTONDOWN
                | WM_MBUTTONDBLCLK
                | WM_MBUTTONUP
                | WM_XBUTTONDOWN
                | WM_XBUTTONDBLCLK
                | WM_XBUTTONUP
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        ) {
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
        // SAFETY: a retired callback may still finish on its old owner thread.
        if unsafe { GetCurrentThreadId() } != s.thread() {
            return false;
        }
        // SAFETY: keyboard focus query takes no arguments.
        if unsafe { GetFocus() } as usize != s.hwnd() {
            return false;
        }
        service(s);
        let scan = ((l as u64 >> 16) & 255) as i32;
        let extended = l as u64 & (1 << 24) != 0;
        let down = l as u64 & (1 << 31) == 0;
        let repeat = l as u64 & (1 << 30) != 0;
        // Our release updates game state, not the physical-held quarantine.
        // SAFETY: reads this thread's current message metadata.
        if !down && unsafe { GetMessageExtraInfo() } as u32 == crate::raw::RELEASE_TAG {
            return false;
        }
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
        let Some(e) = s.ingress(InputRecord::key(vk, scan, down, extended, repeat, modifiers), false) else {
            return false;
        };
        let pass = e.pass;
        let translate = down && e.host.iter().flatten().any(|r| r.kind == 1);
        s.effects(e);
        if translate {
            let msg = MSG {
                hwnd: s.hwnd() as HWND,
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
        // SAFETY: a retired callback may still finish on its old owner thread.
        if unsafe { GetCurrentThreadId() } != s.thread() {
            return false;
        }
        // SAFETY: nonnegative mouse hook supplies a MOUSEHOOKSTRUCTEX.
        let m = unsafe { &*(l as *const MOUSEHOOKSTRUCTEX) };
        if m.Base.hwnd as usize != s.hwnd() {
            return false;
        }
        if m.Base.dwExtraInfo as u32 == crate::raw::RELEASE_TAG
            && matches!(w as u32, WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP | WM_XBUTTONUP)
        {
            return false;
        }
        service(s);
        let mut point = POINT { x: m.Base.pt.x, y: m.Base.pt.y };
        // SAFETY: selected valid window and writable point.
        unsafe {
            ScreenToClient(s.hwnd() as HWND, &mut point);
        }
        let modifiers = s.policy.try_lock().map(|p| p.modifiers()).unwrap_or(0);
        let r = match w as u32 {
            WM_MOUSEMOVE => InputRecord::motion(point.x, point.y, modifiers),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONUP => {
                InputRecord::button(0, w as u32 != WM_LBUTTONUP, point.x, point.y, modifiers)
            }
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK | WM_MBUTTONUP => {
                InputRecord::button(1, w as u32 != WM_MBUTTONUP, point.x, point.y, modifiers)
            }
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_RBUTTONUP => {
                InputRecord::button(2, w as u32 != WM_RBUTTONUP, point.x, point.y, modifiers)
            }
            WM_XBUTTONDOWN | WM_XBUTTONDBLCLK | WM_XBUTTONUP => InputRecord::button(
                2 + ((m.mouseData >> 16) & 0xffff) as i32,
                w as u32 != WM_XBUTTONUP,
                point.x,
                point.y,
                modifiers,
            ),
            WM_MOUSEWHEEL => InputRecord::wheel(0, (m.mouseData >> 16) as i16 as i32, point.x, point.y, modifiers),
            WM_MOUSEHWHEEL => InputRecord::wheel((m.mouseData >> 16) as i16 as i32, 0, point.x, point.y, modifiers),
            _ => return false,
        };
        let Some(e) = s.ingress(r, false) else { return false };
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
    let hwnd = s.hwnd();
    let epoch = s.epoch.load(Ordering::Acquire);
    let valid = || {
        s.hwnd() == hwnd && s.epoch.load(Ordering::Acquire) == epoch
        && s.release.load(Ordering::Acquire) == 0
        // SAFETY: scalar foreground/window queries, including after arbitrary game callbacks.
        && unsafe { GetForegroundWindow() as usize == hwnd && IsWindow(hwnd as HWND) != 0 }
    };
    if !valid() {
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
            let vk = match r.a {
                160 | 161 => 16,
                162 | 163 => 17,
                164 | 165 => 18,
                vk => vk,
            };
            original(s.hwnd() as HWND, WM_KEYUP, vk as usize, l);
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
            let message = match r.a {
                0 => WM_LBUTTONUP,
                1 => WM_MBUTTONUP,
                2 => WM_RBUTTONUP,
                _ => WM_XBUTTONUP,
            };
            let position = ((r.b as u16 as u32) | ((r.c as u16 as u32) << 16)) as isize;
            original(s.hwnd() as HWND, message, if r.a >= 3 { (data as usize) << 16 } else { 0 }, position);
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
        if !valid() {
            return false;
        }
        count += 1;
    }
    if !valid() {
        return false;
    }
    // SAFETY: initialized INPUT prefix, correct native ABI size, foreground verified.
    count == 0
        || unsafe { SendInput(count as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) } == count as u32
}

pub(super) fn observe(s: &Shared, record: InputRecord) -> bool {
    if lowlevel::active() {
        return s.capture_domain(record.kind != 1) || (record.kind == 1 && s.blocks_key(record.a));
    }
    // SAFETY: current thread query has no arguments.
    if unsafe { GetCurrentThreadId() } == s.thread() {
        if let Some(effects) = s.ingress(record, true) {
            let blocked = !effects.pass;
            s.effects(effects);
            return blocked;
        }
    } else {
        s.submit(PolicyEvent::Observed(record));
    }
    s.capture_domain(record.kind != 1) || (record.kind == 1 && s.blocks_key(record.a))
}
