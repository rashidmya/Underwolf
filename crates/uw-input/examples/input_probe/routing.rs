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
static SUPERSEDE_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SEQUENCE_HANDLE: std::sync::OnceLock<(InputHandle, u64)> = std::sync::OnceLock::new();
static GAME_KEYS: AtomicUsize = AtomicUsize::new(0);
static PARTIAL_QUERY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static MOUSE_LEAK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RAW_KEYS: AtomicUsize = AtomicUsize::new(0);
static RAW_MOUSE: AtomicUsize = AtomicUsize::new(0);
static ROLLBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PACKET_COUNT: AtomicUsize = AtomicUsize::new(0);
static PACKET_EDGES: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];
unsafe extern "system" fn packet_observer(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: native low-level observation retains this structure until callback return.
        let key = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        if key.vkCode == VK_PACKET as u32 {
            let index = PACKET_COUNT.fetch_add(1, Ordering::AcqRel);
            if let Some(edge) = PACKET_EDGES.get(index) {
                edge.store((u64::from(key.flags) << 32) | w as u64, Ordering::Release);
            }
        }
    }
    // SAFETY: this independent observer never consumes or changes an input event.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}
unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_LBUTTONDOWN {
        CLICK_TARGET.store(h as usize, Ordering::Release);
    }
    if msg == WM_KEYUP && w == 87 && SUPERSEDE_REQUEST.swap(false, Ordering::AcqRel) {
        let (handle, epoch) = SEQUENCE_HANDLE.get().unwrap();
        handle.set_visible_state(true, 2);
        handle
            .route_request(RoutingRequest {
                epoch: *epoch,
                revision: 1,
                request_id: 3,
                target: None,
                action: RoutingAction::LeaveEdit,
                state_seq: Some(2),
            })
            .unwrap();
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
    // SAFETY: fixture-owned UI thread's active layout; preserve actual scan/extended metadata.
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
    let i = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: (scan & 255) as u16,
                dwFlags: (if down { 0 } else { KEYEVENTF_KEYUP })
                    | (if scan & 0xff00 == 0xe000 { KEYEVENTF_EXTENDEDKEY } else { 0 }),
                ..Default::default()
            },
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
struct RawWorker {
    hwnd: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RawWorker {
    fn start() -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            // SAFETY: this scratch worker owns its message-only window and its input queue.
            unsafe {
                let module = GetModuleHandleW(std::ptr::null());
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: module,
                    lpszClassName: w!("UnderwolfRawWorker"),
                    ..Default::default()
                };
                assert_ne!(RegisterClassW(&class), 0);
                let hwnd = CreateWindowExW(
                    0,
                    class.lpszClassName,
                    w!("Raw worker"),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    module,
                    std::ptr::null(),
                );
                assert!(!hwnd.is_null());
                let devices = [
                    RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
                    RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_NOLEGACY, hwndTarget: hwnd },
                ];
                assert_ne!(
                    RegisterRawInputDevices(devices.as_ptr(), 2, std::mem::size_of::<RAWINPUTDEVICE>() as u32),
                    0
                );
                tx.send(hwnd as usize).unwrap();
                while !stopping.load(Ordering::Acquire) {
                    pump();
                    std::thread::sleep(Duration::from_millis(2));
                }
                DestroyWindow(hwnd);
            }
        });
        Self { hwnd: rx.recv_timeout(Duration::from_secs(3)).unwrap(), stop, thread: Some(thread) }
    }
}
impl Drop for RawWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
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
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
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
    let raw_worker = case.starts_with("raw-thread").then(RawWorker::start);
    if let Some(worker) = &raw_worker {
        // SAFETY: this fixture owns the live worker window and only queries its thread identity.
        assert_ne!(unsafe { GetWindowThreadProcessId(worker.hwnd as HWND, std::ptr::null_mut()) }, unsafe {
            windows_sys::Win32::System::Threading::GetCurrentThreadId()
        });
    }
    if raw_mode && raw_worker.is_none() {
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
    if case == "first-routing-state" {
        h.connect();
        h.set_visible(true);
        drive(&h, 100);
    }
    h.connect_routing();
    h.set_visible(true);
    if case == "first-routing-state" {
        std::thread::sleep(Duration::from_millis(110));
        let first = h.drain();
        assert!(!first.iter().any(|event| matches!(event, RuntimeEvent::RoutingState(_))));
        assert!(h.challenge().is_none(), "legacy state authorized a v2 challenge before a routing state");
        h.disconnect();
        drive(&h, 30);
        // SAFETY: only this fixture's live window is destroyed.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe first-routing-state");
        return;
    }
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
        .chain(matches!(case.as_str(), "partial" | "focus-text" | "resize").then_some(WidgetSpec {
            instance: WidgetRef { id: 2, generation: 1 },
            x: if case == "resize" { 500 } else { 200 },
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
    if case == "resize" {
        let request = |id| RoutingRequest {
            epoch,
            revision: 1,
            request_id: id,
            target: Some(WidgetRef { id: 2, generation: 1 }),
            action: RoutingAction::Takeover,
            state_seq: None,
        };
        h.route_request(request(2)).unwrap();
        let entered = drive(&h, 70);
        assert!(entered.iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(result) if result.request_id == 2 && result.code == RoutingCode::Applied)));
        key(true);
        drive(&h, 30);
        // SAFETY: shrink only this scratch window on its UI owner thread.
        assert_ne!(unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 350, 350, SWP_NOMOVE | SWP_NOZORDER) }, 0);
        let resized = drive(&h, 100);
        assert!(!h.capture_permitted(), "native resize retained capture for an invisible widget: {resized:?}");
        assert!(resized.iter().any(|event| matches!(event, RuntimeEvent::RoutingState(state) if state.focus.is_none() && state.domains == Domains::default())));
        h.route_request(request(3)).unwrap();
        let refused = drive(&h, 70);
        assert!(refused.iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(result) if result.request_id == 3 && result.code != RoutingCode::Applied)), "out-of-client takeover was accepted: {refused:?}");
        key(false);
        drive(&h, 30);
        // SAFETY: restore only the fixture's own window size.
        assert_ne!(unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 640, 480, SWP_NOMOVE | SWP_NOZORDER) }, 0);
        drive(&h, 150);
        assert!(!h.capture_permitted(), "geometry recovery automatically recaptured");
        h.route_request(request(4)).unwrap();
        assert!(drive(&h, 70).iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(result) if result.request_id == 4 && result.code == RoutingCode::Applied)));
        h.disconnect();
        drive(&h, 30);
        // SAFETY: destroy only the fixture's window.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe resize");
        return;
    }
    if let Some(fixture) = directinput {
        fixture.exercise(&h, epoch, configured);
        h.disconnect();
        drive(&h, 50);
        // SAFETY: destroy only this fixture's window after native objects are released.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe {case}");
        return;
    }
    if case == "sequence-reentrant" {
        assert!(SEQUENCE_HANDLE.set((h.clone(), epoch)).is_ok());
        h.set_visible_state(true, 1);
        key_vk(87, true);
        drive(&h, 30);
        SUPERSEDE_REQUEST.store(true, Ordering::Release);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: Some(1),
        })
        .unwrap();
        let events = drive(&h, 100);
        assert!(!SUPERSEDE_REQUEST.load(Ordering::Acquire), "game cleanup callback did not run");
        assert!(events.iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(r) if r.request_id == 2 && r.code == RoutingCode::Superseded)), "reentrant state change did not supersede native entry: {events:?}");
        assert!(!h.capture_permitted());
        key_vk(87, false);
        drive(&h, 30);
        h.disconnect();
        drive(&h, 30);
        // SAFETY: destroy only the fixture's window.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe sequence-reentrant");
        return;
    }
    if case == "sequence" {
        h.set_visible_state(true, 1);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 2,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: Some(1),
        })
        .unwrap();
        h.set_visible_state(true, 2);
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 3,
            target: None,
            action: RoutingAction::LeaveEdit,
            state_seq: Some(2),
        })
        .unwrap();
        let events = drive(&h, 80);
        assert!(events.iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.request_id == 2 && r.code == RoutingCode::Superseded)), "old queued edit-on applied after a newer state: {events:?}");
        assert!(!h.capture_permitted());
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 4,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: Some(999),
        })
        .unwrap();
        let events = drive(&h, 60);
        assert!(events.iter().any(|e| matches!(e, RuntimeEvent::RoutingResult(r) if r.request_id == 4 && r.code == RoutingCode::Superseded)), "a request invented an unobserved state sequence");
        assert!(!h.capture_permitted());
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 5,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        })
        .unwrap();
        assert!(drive(&h, 60).iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(result) if result.request_id == 5 && result.code == RoutingCode::Applied)));
        assert!(h.capture_permitted());
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: 6,
            target: None,
            action: RoutingAction::LeaveEdit,
            state_seq: Some(999),
        })
        .unwrap();
        assert!(drive(&h, 60).iter().any(|event| matches!(event, RuntimeEvent::RoutingResult(result) if result.request_id == 6 && result.code == RoutingCode::Superseded)));
        assert!(h.capture_permitted(), "an invalid future sequence released current ownership");
        h.disconnect();
        drive(&h, 30);
        // SAFETY: destroy only this fixture's live window.
        unsafe {
            DestroyWindow(hwnd);
        }
        println!("PASS routing probe sequence");
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
    let focused = drive(&h, 40);
    if case == "raw-thread-notify" {
        assert!(focused.iter().any(|event| matches!(event, RuntimeEvent::RoutingState(state) if state.focus == Some(WidgetRef { id: 1, generation: 1 }))), "notification fixture never focused its visible widget: {focused:?}");
    }
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
    if matches!(case.as_str(), "exclusive" | "optional-load" | "optional-order") {
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
    let mut events = drive(&h, 60);
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
    events.extend(drive(&h, 80));
    if matches!(case.as_str(), "raw-block" | "raw-thread" | "raw-thread-notify" | "raw-notify") {
        let chars = |events: &[RuntimeEvent]| -> Vec<i32> {
            events
                .iter()
                .filter_map(
                    |event| if let RuntimeEvent::RoutedBatch(batch) = event { Some(batch.events.iter()) } else { None },
                )
                .flatten()
                .filter_map(|event| event.input.filter(|input| input.kind == 2).map(|input| input.a))
                .collect()
        };
        assert_eq!(chars(&events), vec![97], "canonical raw A cycle produced missing or duplicate text: {events:?}");
        key_vk(160, true);
        drive(&h, 25);
        key(true);
        let mut shifted = drive(&h, 30);
        key(false);
        shifted.extend(drive(&h, 60));
        key_vk(160, false);
        shifted.extend(drive(&h, 30));
        assert_eq!(chars(&shifted), vec![65], "canonical raw Shift+A text was missing or duplicated: {shifted:?}");
        // SAFETY: the layout belongs only to this private probe's UI thread.
        let original_layout = unsafe { GetKeyboardLayout(0) };
        // SAFETY: static French KLID, activated only inside this scratch process.
        let layout = unsafe { LoadKeyboardLayoutW(w!("0000040c"), KLF_ACTIVATE) };
        assert!(!layout.is_null(), "native layout fixture unavailable");
        drive(&h, 80);
        let mut expected = [0u16; 8];
        let state = [0u8; 256];
        // SAFETY: independent native translation establishes the selected layout's actual scalar.
        let count = unsafe { ToUnicodeEx(0x32, 0, state.as_ptr(), expected.as_mut_ptr(), 8, 0, layout) };
        assert!(count > 0);
        key_vk(0x32, true);
        let mut localized = drive(&h, 30);
        key_vk(0x32, false);
        localized.extend(drive(&h, 60));
        assert_eq!(
            chars(&localized),
            expected[..count as usize].iter().map(|unit| *unit as i32).collect::<Vec<_>>(),
            "raw text ignored the owner keyboard layout: {localized:?}"
        );
        // SAFETY: restore this fixture's previous layout before the remaining Unicode packet checks.
        unsafe {
            ActivateKeyboardLayout(original_layout, 0);
        }
        // SAFETY: scratch owner-thread observer sees the original edges before production's hook.
        let observer =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(packet_observer), GetModuleHandleW(std::ptr::null()), 0) };
        assert!(!observer.is_null());
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
        // SAFETY: release only this private fixture's observer.
        unsafe {
            UnhookWindowsHookEx(observer);
        }
        let observed: Vec<_> = PACKET_EDGES
            .iter()
            .take(PACKET_COUNT.load(Ordering::Acquire).min(4))
            .map(|edge| edge.load(Ordering::Acquire))
            .collect();
        assert_eq!(
            chars(&events),
            vec![0xe9],
            "native Unicode packet lost or duplicated text; original LL edges {observed:x?}: {events:?}"
        );
    }
    if matches!(case.as_str(), "optional-load" | "optional-order") {
        assert!(h.capture_permitted());
        // SAFETY: load a system input runtime into this private probe process only.
        assert!(!unsafe { LoadLibraryW(w!("dinput8.dll")) }.is_null());
        assert!(!h.capture_permitted(), "optional load must revoke before discovery");
        let events = if case == "optional-order" {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(250) {
                pump();
                std::thread::sleep(Duration::from_millis(2));
            }
            let events = h.drain();
            let mut announced = epoch;
            for event in &events {
                match event {
                    RuntimeEvent::RoutingState(state) => announced = announced.max(state.epoch),
                    RuntimeEvent::NativeCoverage(coverage) => {
                        assert!(coverage.epoch <= announced, "coverage preceded its epoch state: {events:?}")
                    }
                    _ => {}
                }
            }
            events
        } else {
            drive(&h, 150)
        };
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
