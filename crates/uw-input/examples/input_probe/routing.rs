//! Independent real-API driver for the production v2 routing runtime.
#[path = "directinput.rs"]
mod directinput;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use uw_input::{
    configuration::PreparedConfig,
    win::{InputHandle, InputRuntime, RuntimeEvent},
};
use uw_pipe_proto::{input::*, routing::*};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::ClientToScreen,
        System::LibraryLoader::*,
        UI::{Input::KeyboardAndMouse::*, Input::*, WindowsAndMessaging::*},
    },
    core::w,
};
static CLICK_TARGET: AtomicUsize = AtomicUsize::new(0);
static GAME_KEYS: AtomicUsize = AtomicUsize::new(0);
static PARTIAL_QUERY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static MOUSE_LEAK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RAW_KEYS: AtomicUsize = AtomicUsize::new(0);
static RAW_MOUSE: AtomicUsize = AtomicUsize::new(0);
static ROLLBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_LBUTTONDOWN {
        CLICK_TARGET.store(h as usize, Ordering::Release);
    }
    if msg == WM_KEYUP && w == 87 && PARTIAL_QUERY.load(Ordering::Acquire) {
        // SAFETY: real state query during reentrant game cleanup for a continuing mouse domain.
        if unsafe { GetAsyncKeyState(2) } & i16::MIN != 0 {
            MOUSE_LEAK.store(true, Ordering::Release);
        }
    }
    if msg == WM_KEYDOWN && w == 65 {
        GAME_KEYS.fetch_add(1, Ordering::Relaxed);
    }
    if msg == WM_KEYUP && w == 65 && ROLLBACK.load(Ordering::Acquire) {
        let r = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_NOLEGACY, hwndTarget: h };
        // SAFETY: game callback deliberately updates its valid registration during native entry.
        assert_ne!(unsafe { RegisterRawInputDevices(&r, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }, 0);
    }
    if msg == WM_INPUT {
        let mut bytes = 0;
        // SAFETY: documented raw size query for this live WM_INPUT handle.
        assert_eq!(
            // SAFETY: documented raw size query for this live WM_INPUT handle.
            unsafe {
                GetRawInputData(
                    l as HRAWINPUT,
                    RID_INPUT,
                    std::ptr::null_mut(),
                    &mut bytes,
                    std::mem::size_of::<RAWINPUTHEADER>() as u32,
                )
            },
            0
        );
        let mut raw = RAWINPUT::default();
        let mut capacity = std::mem::size_of::<RAWINPUT>() as u32;
        // SAFETY: initialized raw storage has at least the size returned for keyboard/mouse input.
        assert!(bytes <= capacity);
        // SAFETY: raw storage is large enough for the queried native record.
        let read = unsafe {
            GetRawInputData(
                l as HRAWINPUT,
                RID_INPUT,
                (&mut raw as *mut RAWINPUT).cast(),
                &mut capacity,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        assert_eq!(read, bytes, "filter changed original raw return size");
        // SAFETY: dwType selects the initialized union member returned by user32.
        unsafe {
            if raw.header.dwType == RIM_TYPEKEYBOARD && raw.data.keyboard.VKey == 65 && raw.data.keyboard.Flags & 1 == 0
            {
                RAW_KEYS.fetch_add(1, Ordering::Relaxed);
            }
            if raw.header.dwType == RIM_TYPEMOUSE
                && (raw.data.mouse.lLastX != 0 || raw.data.mouse.lLastY != 0 || raw.data.mouse.Anonymous.ulButtons != 0)
            {
                RAW_MOUSE.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    // SAFETY: forward native window-procedure arguments unchanged.
    unsafe { DefWindowProcW(h, msg, w, l) }
}
fn pump() {
    // SAFETY: standard thread-owned message queue and initialized MSG storage.
    unsafe {
        let mut m = MSG::default();
        while PeekMessageW(&mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}
fn key(down: bool) {
    key_vk(65, down);
}
fn key_vk(vk: u16, down: bool) {
    let i = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, dwFlags: if down { 0 } else { KEYEVENTF_KEYUP }, ..Default::default() },
        },
    };
    // SAFETY: one initialized native keyboard input.
    assert_eq!(unsafe { SendInput(1, &i, std::mem::size_of::<INPUT>() as i32) }, 1);
}
fn click(down: bool) {
    let i = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP },
                ..Default::default()
            },
        },
    };
    // SAFETY: one initialized native mouse input.
    assert_eq!(unsafe { SendInput(1, &i, std::mem::size_of::<INPUT>() as i32) }, 1);
}
fn drive(h: &InputHandle, duration: u64) -> Vec<RuntimeEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    while start.elapsed() < Duration::from_millis(duration) {
        pump();
        events.extend(h.drain());
        if let Some(c) = h.challenge() {
            h.acknowledge(InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq });
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    events
}
pub fn run() {
    let case = std::env::args().skip_while(|a| a != "--routing").nth(1).expect("routing case");
    let raw_mode = case.starts_with("raw-");
    // SAFETY: process-owned class and window; callback remains resident.
    let hwnd = unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let c = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: w!("UnderwolfRoutingProbe"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&c), 0);
        let hwnd = CreateWindowExW(
            0,
            c.lpszClassName,
            w!("Routing probe"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            50,
            50,
            640,
            480,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        assert!(!hwnd.is_null());
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        ShowCursor(1);
        hwnd
    };
    if raw_mode {
        let devices = [
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
        ];
        // SAFETY: native descriptors target the fixture's own live window.
        assert_ne!(
            // SAFETY: both initialized descriptors target this fixture's live window.
            unsafe { RegisterRawInputDevices(devices.as_ptr(), 2, std::mem::size_of::<RAWINPUTDEVICE>() as u32) },
            0
        );
    }
    let directinput = if case.starts_with("directinput") { Some(directinput::Fixture::create(hwnd)) } else { None };
    let runtime = InputRuntime::install(hwnd).expect("install");
    let h = runtime.handle();
    h.connect_routing();
    h.set_visible(true);
    let events = drive(&h, 200);
    let epoch = events
        .iter()
        .filter_map(|e| if let RuntimeEvent::RoutingState(s) = e { Some(s.epoch) } else { None })
        .next_back()
        .expect("routing ready");
    let config = PreparedConfig::compile(RoutingConfig {
        epoch,
        revision: 1,
        request_id: 1,
        width: 620,
        height: 440,
        widgets: vec![WidgetSpec {
            instance: WidgetRef { id: 1, generation: 1 },
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            z: 1,
            mode: if case.ends_with("notify") { InputMode::Notify } else { InputMode::Block },
            visible: true,
            keyboard: !case.ends_with("mouse-only") && case != "partial",
        }]
        .into_iter()
        .chain(matches!(case.as_str(), "partial" | "focus-text").then_some(WidgetSpec {
            instance: WidgetRef { id: 2, generation: 1 },
            x: 200,
            y: 0,
            width: 100,
            height: 100,
            z: 2,
            mode: InputMode::Block,
            visible: true,
            keyboard: true,
        }))
        .collect(),
        hotkeys: vec![
            HotkeySpec { id: 1, vk: 9, modifiers: 8, flags: 0, action: HotkeyAction::ToggleEdit, target: None },
            HotkeySpec { id: 2, vk: 113, modifiers: 0, flags: 1, action: HotkeyAction::Notify, target: None },
            HotkeySpec { id: 3, vk: 114, modifiers: 0, flags: 0, action: HotkeyAction::Notify, target: None },
        ],
    })
    .unwrap();
    h.configure(config).unwrap();
    let configured = drive(&h, 80);
    assert!(
        configured
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Configured(r) if r.code == RoutingCode::Applied && r.request_id == 1))
    );
    if let Some(fixture) = directinput {
        fixture.exercise(&h, epoch, configured);
        h.disconnect();
        drive(&h, 50);
        // SAFETY: destroy only this fixture's window after native objects are released.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe directinput");
        return;
    }
    if case == "rollback" {
        key(true);
        drive(&h, 40);
        ROLLBACK.store(true, Ordering::Release);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        })
        .unwrap();
        let events = drive(&h, 150);
        assert!(
            events.iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.code == RoutingCode::SetupFailed)),
            "native entry did not report rollback: {events:?}"
        );
        assert!(!h.capture_permitted());
        ROLLBACK.store(false, Ordering::Release);
        key(false);
        drive(&h, 30);
        println!("PASS routing probe rollback");
        return;
    }
    // SAFETY: translate a fixture-local client point into desktop coordinates.
    unsafe {
        let mut p = POINT { x: 20, y: 20 };
        ClientToScreen(hwnd, &mut p);
        SetCursorPos(p.x, p.y);
    }
    drive(&h, 40);
    click(true);
    drive(&h, 40);
    click(false);
    drive(&h, 40);
    if case == "focus-text" {
        // SAFETY: post UTF-16 units only to this fixture's live window.
        unsafe {
            PostMessageW(hwnd, WM_CHAR, 0xd83d, 0);
        }
        drive(&h, 30);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: Some(WidgetRef { id: 2, generation: 1 }),
            action: RoutingAction::Takeover,
            state_seq: None,
        })
        .unwrap();
        let applied = drive(&h, 50);
        assert!(applied.iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.code == RoutingCode::Applied)));
        // SAFETY: the low surrogate arrives after the owner has committed a different target.
        unsafe {
            PostMessageW(hwnd, WM_CHAR, 0xde00, 0);
        }
        let events = drive(&h, 50);
        let chars = events
            .iter()
            .filter_map(|e| if let RuntimeEvent::RoutedBatch(b) = e { Some(b) } else { None })
            .flat_map(|b| &b.events)
            .filter(|e| e.target == Some(WidgetRef { id: 2, generation: 1 }))
            .filter_map(|e| e.input.filter(|r| r.kind == 2).map(|r| r.a))
            .collect::<Vec<_>>();
        assert_eq!(chars, vec![0xfffd], "text decoder paired UTF-16 across a host focus change");
        h.disconnect();
        drive(&h, 30);
        // SAFETY: destroy only this fixture's window.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe focus-text");
        return;
    }
    if case == "partial" {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: MOUSEEVENTF_RIGHTDOWN, ..Default::default() } },
        };
        // SAFETY: initialized held right-button input in the mouse-owned widget.
        assert_eq!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) }, 1);
        drive(&h, 30);
        key_vk(87, true);
        drive(&h, 30);
        PARTIAL_QUERY.store(true, Ordering::Release);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: Some(WidgetRef { id: 2, generation: 1 }),
            action: RoutingAction::Takeover,
            state_seq: None,
        })
        .unwrap();
        assert!(
            drive(&h, 80).iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.code == RoutingCode::Applied))
        );
        PARTIAL_QUERY.store(false, Ordering::Release);
        assert!(!MOUSE_LEAK.load(Ordering::Acquire), "gaining keyboard exposed the continuing mouse domain");
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: MOUSEEVENTF_RIGHTUP, ..Default::default() } },
        };
        // SAFETY: balance only the fixture's own right-button press.
        unsafe {
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        }
        key_vk(87, false);
        drive(&h, 40);
        h.disconnect();
        drive(&h, 40);
        // SAFETY: destroy only this fixture's window.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe partial");
        return;
    }
    if case == "keyboard-only" {
        // SAFETY: fixture-local point outside the widget, inside the same window.
        let point = unsafe {
            let mut p = POINT { x: 200, y: 200 };
            ClientToScreen(hwnd, &mut p);
            p
        };
        // SAFETY: screen metrics and initialized absolute desktop mouse input.
        unsafe {
            let i = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: point.x * 65535 / (GetSystemMetrics(SM_CXSCREEN) - 1),
                        dy: point.y * 65535 / (GetSystemMetrics(SM_CYSCREEN) - 1),
                        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                        ..Default::default()
                    },
                },
            };
            assert_eq!(SendInput(1, &i, std::mem::size_of::<INPUT>() as i32), 1);
        }
        let events = drive(&h, 50);
        assert!(
            events.iter().any(|e| matches!(e, RuntimeEvent::RoutingState(s) if s.domains.keyboard && !s.domains.mouse)),
            "mouse ownership did not end"
        );
        // SAFETY: writable point receives the fixture cursor location.
        let current = unsafe {
            let mut p = POINT::default();
            GetCursorPos(&mut p);
            p
        };
        assert!(
            (current.x - point.x).abs() <= 1 && (current.y - point.y).abs() <= 1,
            "standard exit warped the physical pointer"
        );
    }
    if matches!(case.as_str(), "exclusive" | "optional-load") {
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        })
        .unwrap();
        assert!(
            drive(&h, 50).iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.code == RoutingCode::Applied))
        );
    }
    let mut named = 0;
    for _ in 0..2 {
        key_vk(113, true);
        let mut events = drive(&h, 25);
        key_vk(113, false);
        events.extend(drive(&h, 25));
        named += events
            .iter()
            .filter_map(|e| {
                if let RuntimeEvent::RoutedBatch(b) = e {
                    Some(b.events.iter().filter(|e| e.kind == EventKind::Hotkey && e.argument == 2).count())
                } else {
                    None
                }
            })
            .sum::<usize>();
    }
    assert_eq!(named, 2, "raw/legacy full key cycles duplicated or lost a named activation");
    key_vk(114, true);
    let mut consuming = drive(&h, 25);
    key_vk(114, false);
    consuming.extend(drive(&h, 25));
    let routed: Vec<_> = consuming
        .iter()
        .filter_map(|e| if let RuntimeEvent::RoutedBatch(b) = e { Some(&b.events) } else { None })
        .flatten()
        .collect();
    assert_eq!(routed.iter().filter(|e| e.kind == EventKind::Hotkey && e.argument == 3).count(), 1);
    assert!(
        !routed.iter().any(|e| e.input.is_some_and(|r| r.kind == 1 && r.a == 114)),
        "consuming hotkey left an unmatched widget key-down"
    );
    RAW_KEYS.store(0, Ordering::Release);
    RAW_MOUSE.store(0, Ordering::Release);
    key(true);
    let events = drive(&h, 60);
    // Resolve polling through the real system export independently of production's import stub.
    type Async = unsafe extern "system" fn(i32) -> i16;
    // SAFETY: exact export and ABI for user32 GetAsyncKeyState.
    let held = unsafe {
        let p = GetProcAddress(GetModuleHandleW(w!("user32.dll")), c"GetAsyncKeyState".as_ptr().cast()).unwrap();
        std::mem::transmute::<unsafe extern "system" fn() -> isize, Async>(p)(65) & i16::MIN != 0
    };
    let passing = case.ends_with("mouse-only") || case.ends_with("notify");
    assert_eq!(held, passing, "{case}: polling ownership");
    if raw_mode {
        assert_eq!(RAW_KEYS.load(Ordering::Acquire) != 0, passing, "{case}: raw keyboard");
    } else {
        assert_eq!(GAME_KEYS.load(Ordering::Acquire) != 0, passing, "{case}: keyboard messages");
    }
    if raw_mode {
        let motion = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 1, dy: 1, dwFlags: MOUSEEVENTF_MOVE, ..Default::default() } },
        };
        // SAFETY: initialized relative mouse input remains inside the test widget.
        assert_eq!(unsafe { SendInput(1, &motion, std::mem::size_of::<INPUT>() as i32) }, 1);
        drive(&h, 30);
        assert_eq!(RAW_MOUSE.load(Ordering::Acquire) == 0, !case.ends_with("notify"), "mouse raw domain filtering");
    }
    if case == "notify" {
        assert!(events.iter().any(|e| matches!(e, RuntimeEvent::RoutedBatch(b) if b.events.iter().any(|e| e.input.is_some_and(|r| r.kind == 1 && r.a == 65)))));
    }
    key(false);
    drive(&h, 30);
    if case == "raw-block" {
        for up in [false, true] {
            let i = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wScan: 0xe9,
                        dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                        ..Default::default()
                    },
                },
            };
            // SAFETY: initialized Unicode packet input with native structure size.
            assert_eq!(unsafe { SendInput(1, &i, std::mem::size_of::<INPUT>() as i32) }, 1);
        }
        let events = drive(&h, 60);
        assert!(events.iter().any(|e| matches!(e, RuntimeEvent::RoutedBatch(b) if b.events.iter().any(|e| e.target == Some(WidgetRef { id: 1, generation: 1 }) && e.input.is_some_and(|r| r.kind == 2 && r.a == 0xe9)))), "native Unicode packet lost the focused widget target");
    }
    if case == "optional-load" {
        assert!(h.capture_permitted());
        // SAFETY: load a system input runtime into this private probe process only.
        assert!(!unsafe { LoadLibraryW(w!("dinput8.dll")) }.is_null());
        assert!(!h.capture_permitted(), "optional load must revoke before discovery");
        let events = drive(&h, 150);
        assert!(!h.capture_permitted(), "discovery must never rearm ownership");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::NativeCoverage(c) if c.directinput.status != NativeStatus::Absent))
        );
    }
    h.disconnect();
    drive(&h, 50);
    // SAFETY: destroy only the probe's window.
    unsafe {
        DestroyWindow(hwnd);
    }
    println!("PASS routing probe {case}");
}
