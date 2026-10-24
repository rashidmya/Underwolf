#[cfg(not(windows))]
fn main() {
    eprintln!("input_probe requires Windows/Wine");
}
#[cfg(windows)]
fn main() {
    win::run();
}
#[cfg(windows)]
mod win {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };
    use uw_input::win::{InputRuntime, RuntimeEvent};
    use uw_pipe_proto::input::*;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Input::KeyboardAndMouse::*, Input::*, WindowsAndMessaging::*},
    };
    use windows_sys::core::w;
    static BUFFER_KEYS: AtomicUsize = AtomicUsize::new(0);
    static RAW_MOUSE: AtomicUsize = AtomicUsize::new(0);
    static RAW_KEYS: AtomicUsize = AtomicUsize::new(0);
    static GAME_KEYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_INPUT {
            let mut size = 0u32;
            // SAFETY: message provides an HRAWINPUT; null buffer is the documented size query.
            let query = unsafe {
                GetRawInputData(
                    l as HRAWINPUT,
                    RID_INPUT,
                    std::ptr::null_mut(),
                    &mut size,
                    std::mem::size_of::<RAWINPUTHEADER>() as u32,
                )
            };
            if query != u32::MAX && size <= 256 {
                let mut bytes = [0u64; 32];
                // SAFETY: aligned local buffer is large enough for reported record.
                let n = unsafe {
                    GetRawInputData(
                        l as HRAWINPUT,
                        RID_INPUT,
                        bytes.as_mut_ptr().cast(),
                        &mut size,
                        std::mem::size_of::<RAWINPUTHEADER>() as u32,
                    )
                };
                if n != u32::MAX && n >= 40 {
                    // SAFETY: read only the successfully initialized record bytes.
                    let raw = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), n as usize) };
                    count_mouse(raw);
                    if u32::from_le_bytes(raw[..4].try_into().unwrap()) == 1
                        && u16::from_le_bytes(raw[30..32].try_into().unwrap()) == 65
                        && u16::from_le_bytes(raw[26..28].try_into().unwrap()) & 1 == 0
                    {
                        RAW_KEYS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        if m == WM_KEYDOWN {
            GAME_KEYS.fetch_add(1, Ordering::Relaxed);
        }
        if m == WM_DESTROY {
            // SAFETY: ends this test thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        // SAFETY: delegate test-window messages to default processing.
        unsafe { DefWindowProcW(h, m, w, l) }
    }
    fn count_mouse(raw: &[u8]) {
        if raw.len() >= 48
            && u32::from_le_bytes(raw[..4].try_into().unwrap()) == 0
            && raw[28..44].iter().any(|v| *v != 0)
        {
            RAW_MOUSE.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn mouse(flags: u32, data: u32, dx: i32, dy: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        // SAFETY: one initialized native INPUT.
        assert_eq!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) }, 1);
    }
    fn read_buffer() {
        let mut bytes = [0u64; 2048];
        let mut size = std::mem::size_of_val(&bytes) as u32;
        // SAFETY: aligned initialized buffer with byte capacity and native header size.
        let count = unsafe {
            GetRawInputBuffer(bytes.as_mut_ptr().cast(), &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32)
        };
        assert_ne!(count, u32::MAX, "buffered raw read failed");
        // SAFETY: the whole storage is initialized, including any unused tail.
        let records = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), std::mem::size_of_val(&bytes)) };
        for span in uw_input::raw::RawRecordIter::new(records, count) {
            let record = &records[span.expect("valid native raw record")];
            count_mouse(record);
            if u32::from_le_bytes(record[..4].try_into().unwrap()) == 1
                && u16::from_le_bytes(record[30..32].try_into().unwrap()) == 65
                && u16::from_le_bytes(record[26..28].try_into().unwrap()) & 1 == 0
            {
                BUFFER_KEYS.fetch_add(1, Ordering::Relaxed);
                RAW_KEYS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    fn pump() {
        if std::env::args().any(|a| a == "--buffered") {
            read_buffer();
        }
        // SAFETY: standard owner-thread message loop with writable MSG.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    fn key(vk: u16, down: bool) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: one initialized INPUT structure with correct native size.
        assert_eq!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) }, 1);
    }
    pub fn run() {
        // SAFETY: standard owner-thread class registration/window creation; callback remains resident.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: w!("UnderwolfInputProbe"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let h = CreateWindowExW(
                0,
                class.lpszClassName,
                w!("Input probe"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                50,
                50,
                640,
                480,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            assert!(!h.is_null());
            SetForegroundWindow(h);
            SetFocus(h);
            h
        };
        let mut devices = [
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
        ];
        // SAFETY: owned descriptors point to our window and use documented sizes.
        assert_ne!(
            // SAFETY: descriptors are initialized, with native count and size.
            unsafe { RegisterRawInputDevices(devices.as_ptr(), 2, std::mem::size_of::<RAWINPUTDEVICE>() as u32) },
            0
        );
        let runtime = InputRuntime::install(hwnd).expect("install");
        let h = runtime.handle();
        h.connect();
        h.set_visible(true);
        let started = Instant::now();
        let mut epoch = 0;
        let mut requested = false;
        let mut active = false;
        let mut sent = false;
        let mut host_keys = 0;
        let mut host_chars = 0;
        let mut host_emoji = 0;
        let unicode_test = std::env::args().any(|a| a == "--unicode");
        let mut host_buttons = [0; 5];
        let mut host_wheels = [0; 2];
        let mouse_test = std::env::args().any(|a| a == "--mouse");
        let mut before_mouse = 0;
        let mut last_ack = Instant::now();
        let mut stalled = false;
        let mut released = false;
        let mut before_sent = false;
        let mut before_raw = 0;
        let cursor_test = std::env::args().any(|a| a == "--cursor");
        let stall_ui = std::env::args().any(|a| a == "--stall-ui");
        let mut stalled_once = false;
        let mut baseline_count = 0;
        let intended_clip = windows_sys::Win32::Foundation::RECT { left: 60, top: 60, right: 400, bottom: 400 };
        if cursor_test {
            // SAFETY: paired owner-thread count probe preserves the original count.
            baseline_count = unsafe {
                let count = ShowCursor(1) - 1;
                ShowCursor(0);
                count
            };
        }
        while started.elapsed() < Duration::from_secs(8) {
            pump();
            for event in h.drain() {
                eprintln!("{event:?}");
                match event {
                    RuntimeEvent::State(s) => {
                        epoch = s.epoch;
                        active = s.active;
                        if sent && !s.active {
                            released = true;
                        }
                    }
                    RuntimeEvent::Batch(b) => {
                        for r in b.records {
                            if r.kind == 1 && r.a == 65 && r.flags & 1 != 0 {
                                host_keys += 1
                            }
                            if r.kind == 2 && r.a == 97 {
                                host_chars += 1
                            }
                            if r.kind == 2 && r.a == 0x1f600 {
                                host_emoji += 1
                            }
                            if r.kind == 4 && r.flags & 1 != 0 {
                                host_buttons[r.a as usize] += 1;
                            }
                            if r.kind == 5 {
                                host_wheels[0] += (r.a != 0) as i32;
                                host_wheels[1] += (r.b != 0) as i32;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !stalled && let Some(c) = h.challenge() {
                h.acknowledge(InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq });
                last_ack = Instant::now();
            }
            if !before_sent && started.elapsed() > Duration::from_millis(200) {
                key(65, true);
                for _ in 0..4 {
                    pump();
                    std::thread::sleep(Duration::from_millis(5));
                }
                // SAFETY: scalar query proves real pressed state exists before capture.
                assert!(unsafe { GetAsyncKeyState(65) } < 0, "no physical pressed-state baseline");
                key(65, false);
                if mouse_test {
                    mouse(MOUSEEVENTF_MOVE, 0, 1, 1);
                }
                before_sent = true;
            }
            if epoch != 0 && !requested && started.elapsed() > Duration::from_millis(500) {
                before_raw = RAW_KEYS.load(Ordering::Relaxed);
                before_mouse = RAW_MOUSE.load(Ordering::Relaxed);
                if mouse_test {
                    assert!(before_mouse > 0, "no real raw mouse baseline");
                }
                assert!(before_raw > 0, "probe saw no real raw input before capture");
                if std::env::args().any(|a| a == "--buffered") {
                    assert!(BUFFER_KEYS.load(Ordering::Relaxed) > 0, "buffered probe never observed real input");
                }
                if std::env::args().any(|a| a == "--hotkey") {
                    key(162, true);
                    key(9, true);
                    key(9, false);
                    key(162, false);
                } else {
                    h.request(CaptureRequest { epoch, state_seq: 1, request_id: 1, on: true }).unwrap();
                }
                requested = true;
            }
            if active && !sent {
                if std::env::args().any(|a| a == "--overflow") {
                    for id in 2..34 {
                        h.request(CaptureRequest { epoch, state_seq: 1, request_id: id, on: true }).unwrap();
                    }
                    pump();
                    assert!(
                        h.drain().iter().any(|e| matches!(e, RuntimeEvent::Disconnect)),
                        "control overflow did not close the session"
                    );
                    assert!(!h.capture_permitted());
                    // SAFETY: this test owns the window.
                    unsafe {
                        DestroyWindow(hwnd);
                    }
                    println!("PASS input probe: control overflow disconnects");
                    return;
                }
                // SAFETY: writable buffers and scalar Win32 test requests.
                unsafe {
                    let mut keys = [0u8; 256];
                    assert_ne!(GetKeyboardState(keys.as_mut_ptr()), 0);
                    assert!(keys.iter().all(|key| key & 0x80 == 0));
                    assert_eq!(GetAsyncKeyState(65), 0);
                    if cursor_test {
                        std::thread::spawn(|| {
                            // SAFETY: this independent thread adjusts only its own input-queue count.
                            let before = ShowCursor(1) - 1;
                            ShowCursor(0);
                            assert_eq!(ShowCursor(0), before - 1);
                            assert_eq!(ShowCursor(1), before);
                        })
                        .join()
                        .unwrap();
                        assert!(h.capture_permitted(), "independent cursor thread revoked capture");
                        let mut frozen = windows_sys::Win32::Foundation::POINT::default();
                        assert_ne!(GetCursorPos(&mut frozen), 0);
                        assert_ne!(SetCursorPos(150, 150), 0);
                        let mut game = windows_sys::Win32::Foundation::POINT::default();
                        assert_ne!(GetCursorPos(&mut game), 0);
                        assert_eq!((game.x, game.y), (frozen.x, frozen.y));
                        assert_ne!(ClipCursor(&intended_clip), 0);
                        let mut clip = windows_sys::Win32::Foundation::RECT::default();
                        assert_ne!(GetClipCursor(&mut clip), 0);
                        assert_eq!((clip.left, clip.top, clip.right, clip.bottom), (60, 60, 400, 400));
                        assert_eq!(ShowCursor(0), baseline_count - 1);
                        devices[1].dwFlags |= RIDEV_INPUTSINK;
                        assert_ne!(
                            RegisterRawInputDevices(devices.as_ptr(), 2, std::mem::size_of::<RAWINPUTDEVICE>() as u32),
                            0
                        );
                    }
                }
                key(65, true);
                for _ in 0..4 {
                    pump();
                    std::thread::sleep(Duration::from_millis(5));
                }
                // SAFETY: valid keyboard state buffer and scalar polling calls during a real held key.
                unsafe {
                    assert_eq!(GetAsyncKeyState(65), 0, "async pressed state leaked");
                    assert_eq!(GetKeyState(65) & i16::MIN, 0, "key pressed state leaked");
                    let mut state = [0u8; 256];
                    assert_ne!(GetKeyboardState(state.as_mut_ptr()), 0);
                    assert!(state.iter().all(|v| v & 0x80 == 0), "keyboard pressed state leaked");
                }
                key(65, false);
                if mouse_test {
                    mouse(MOUSEEVENTF_MOVE, 0, 2, 2);
                    for (down, up, data) in [
                        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
                        (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
                        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
                        (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
                        (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
                    ] {
                        mouse(down, data, 0, 0);
                        mouse(up, data, 0, 0);
                    }
                    mouse(MOUSEEVENTF_WHEEL, 120, 0, 0);
                    mouse(MOUSEEVENTF_HWHEEL, 120, 0, 0);
                }
                if unicode_test {
                    for unit in [0xd83d, 0xde00] {
                        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                            let input = INPUT {
                                r#type: INPUT_KEYBOARD,
                                Anonymous: INPUT_0 {
                                    ki: KEYBDINPUT { wVk: 0, wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                                },
                            };
                            // SAFETY: one native Unicode keyboard packet.
                            assert_eq!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) }, 1);
                        }
                    }
                }
                sent = true;
            }
            if sent
                && host_keys == 1
                && host_chars == 1
                && (!unicode_test || host_emoji == 1)
                && (!mouse_test || (host_buttons == [1; 5] && host_wheels == [1; 2]))
            {
                if mouse_test && !stalled {
                    assert_eq!(RAW_MOUSE.load(Ordering::Relaxed), before_mouse, "captured raw mouse leaked");
                }
                stalled = true;
                if std::env::args().any(|a| a == "--destroy") {
                    // SAFETY: this probe owns the window and writable rectangle.
                    unsafe {
                        DestroyWindow(hwnd);
                        assert!(!h.capture_permitted());
                        let mut clip = windows_sys::Win32::Foundation::RECT::default();
                        assert_ne!(GetClipCursor(&mut clip), 0);
                        assert_ne!(
                            (clip.left, clip.top, clip.right, clip.bottom),
                            (60, 60, 400, 400),
                            "destroy restored stale clip"
                        );
                    }
                    println!("PASS input probe: destruction releases confinement");
                    return;
                }
                if stall_ui && !stalled_once {
                    stalled_once = true;
                    std::thread::sleep(Duration::from_millis(700));
                    assert!(!h.capture_permitted(), "stalled UI retained capture permission");
                    assert!(
                        !h.drain().iter().any(|event| matches!(event, RuntimeEvent::Applied(_))),
                        "UI stall fabricated an application acknowledgement"
                    );
                }
            }
            if stalled && released {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sent, "capture was never entered");
        if mouse_test {
            assert_eq!(host_buttons, [1; 5], "five buttons exactly once");
            assert_eq!(host_wheels, [1; 2], "two wheel axes exactly once");
        }
        assert_eq!(host_keys, 1, "one host key down");
        assert_eq!(host_chars, 1, "one host character");
        if unicode_test {
            assert_eq!(host_emoji, 1, "one decoded surrogate pair");
        }
        assert_eq!(GAME_KEYS.load(Ordering::Relaxed), 0, "game received captured key");
        assert_eq!(RAW_KEYS.load(Ordering::Relaxed), before_raw, "raw key leaked during capture");
        assert!(stalled && released, "watchdog failed to release");
        assert!(last_ack.elapsed() < Duration::from_secs(2), "release took too long");
        assert!(!h.capture_permitted());
        let mut count = 16u32;
        let mut restored = [RAWINPUTDEVICE::default(); 16];
        // SAFETY: writable fixed descriptor array and count.
        let n = unsafe {
            GetRegisteredRawInputDevices(
                restored.as_mut_ptr(),
                &mut count,
                std::mem::size_of::<RAWINPUTDEVICE>() as u32,
            )
        };
        assert_ne!(n, u32::MAX);
        for original in devices {
            assert!(
                restored[..n as usize].iter().any(|r| r.usUsagePage == original.usUsagePage
                    && r.usUsage == original.usUsage
                    && r.dwFlags == original.dwFlags
                    && r.hwndTarget == original.hwndTarget),
                "raw registration not restored"
            );
        }
        if cursor_test {
            // SAFETY: writable test outputs; restore count/clip after checking latest intent.
            unsafe {
                let mut point = windows_sys::Win32::Foundation::POINT::default();
                assert_ne!(GetCursorPos(&mut point), 0);
                assert_eq!((point.x, point.y), (150, 150));
                let mut clip = windows_sys::Win32::Foundation::RECT::default();
                assert_ne!(GetClipCursor(&mut clip), 0);
                assert_eq!((clip.left, clip.top, clip.right, clip.bottom), (60, 60, 400, 400));
                assert_eq!(ShowCursor(1), baseline_count);
                ClipCursor(std::ptr::null());
            }
        }
        key(65, true);
        key(65, false);
        for _ in 0..20 {
            pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(RAW_KEYS.load(Ordering::Relaxed) > before_raw, "raw input did not resume");
        // SAFETY: this test owns the window.
        unsafe { DestroyWindow(hwnd) };
        println!("PASS input probe: capture, text once, game suppression, lease release");
    }
}
