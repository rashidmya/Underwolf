#[cfg(not(windows))]
fn main() {
    eprintln!("input_probe requires Windows/Wine");
}
#[cfg(windows)]
#[path = "input_probe/routing.rs"]
mod routing_probe;
#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--routing") {
        routing_probe::run();
        return;
    }
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
    static API_EXPORTS: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::args().any(|a| a == "--api-exports"));
    macro_rules! system_api {
        ($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty, $native:path) => {
            #[allow(non_snake_case)]
            unsafe extern "system" fn $name($($arg: $ty),*) -> $ret {
                static CALL: std::sync::LazyLock<unsafe extern "system" fn($($ty),*) -> $ret> = std::sync::LazyLock::new(|| {
                    if *API_EXPORTS {
                        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
                        let name = concat!(stringify!($name), "\0");
                        // SAFETY: native export name and exact signature, resolved independently of uw-input.
                        unsafe {
                            let module = GetModuleHandleW(w!("user32.dll"));
                            let export = GetProcAddress(module, name.as_ptr()).expect("required probe export");
                            std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn($($ty),*) -> $ret>(export)
                        }
                    } else { $native }
                });
                // SAFETY: the probe caller supplies the corresponding Win32 API's arguments.
                unsafe { CALL($($arg),*) }
            }
        };
    }
    system_api!(GetKeyState(vk: i32) -> i16, windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState);
    system_api!(GetAsyncKeyState(vk: i32) -> i16, windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState);
    system_api!(GetKeyboardState(out: *mut u8) -> i32, windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState);
    system_api!(GetCursorPos(out: *mut windows_sys::Win32::Foundation::POINT) -> i32, windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos);
    system_api!(SetCursorPos(x: i32, y: i32) -> i32, windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos);
    system_api!(ClipCursor(rect: *const windows_sys::Win32::Foundation::RECT) -> i32, windows_sys::Win32::UI::WindowsAndMessaging::ClipCursor);
    system_api!(GetClipCursor(rect: *mut windows_sys::Win32::Foundation::RECT) -> i32, windows_sys::Win32::UI::WindowsAndMessaging::GetClipCursor);
    system_api!(ShowCursor(on: i32) -> i32, windows_sys::Win32::UI::WindowsAndMessaging::ShowCursor);
    system_api!(SetCursor(shape: HCURSOR) -> HCURSOR, windows_sys::Win32::UI::WindowsAndMessaging::SetCursor);
    system_api!(GetCursor() -> HCURSOR, windows_sys::Win32::UI::WindowsAndMessaging::GetCursor);
    system_api!(RegisterRawInputDevices(devices: *const RAWINPUTDEVICE, count: u32, size: u32) -> i32, windows_sys::Win32::UI::Input::RegisterRawInputDevices);
    type DataFn = unsafe extern "system" fn(HRAWINPUT, u32, *mut core::ffi::c_void, *mut u32, u32) -> u32;
    type BufferFn = unsafe extern "system" fn(*mut RAWINPUT, *mut u32, u32) -> u32;
    fn raw_exports() -> &'static (DataFn, BufferFn) {
        static EXPORTS: std::sync::OnceLock<(DataFn, BufferFn)> = std::sync::OnceLock::new();
        EXPORTS.get_or_init(|| {
            use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
            // SAFETY: user32 is already loaded; both static names are ABI-matching native exports.
            unsafe {
                let module = GetModuleHandleW(w!("user32.dll"));
                let data = GetProcAddress(module, c"GetRawInputData".as_ptr().cast()).unwrap();
                let buffer = GetProcAddress(module, c"GetRawInputBuffer".as_ptr().cast()).unwrap();
                (
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, DataFn>(data),
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, BufferFn>(buffer),
                )
            }
        })
    }
    unsafe fn read_raw_data(
        input: HRAWINPUT,
        command: u32,
        out: *mut core::ffi::c_void,
        size: *mut u32,
        header: u32,
    ) -> u32 {
        let call = if *API_EXPORTS || std::env::args().any(|a| a == "--raw-exports") {
            raw_exports().0
        } else {
            GetRawInputData
        };
        // SAFETY: forwards the native caller's unchanged buffers and scalar API arguments.
        unsafe { call(input, command, out, size, header) }
    }
    unsafe fn read_raw_buffer(out: *mut RAWINPUT, size: *mut u32, header: u32) -> u32 {
        let call = if *API_EXPORTS || std::env::args().any(|a| a == "--raw-exports") {
            raw_exports().1
        } else {
            GetRawInputBuffer
        };
        // SAFETY: forwards the native caller's unchanged output array and size/header arguments.
        unsafe { call(out, size, header) }
    }
    static BUFFER_KEYS: AtomicUsize = AtomicUsize::new(0);
    static RAW_W_DOWN: AtomicUsize = AtomicUsize::new(0);
    static RAW_W_UP: AtomicUsize = AtomicUsize::new(0);
    static RAW_MOUSE: AtomicUsize = AtomicUsize::new(0);
    static RAW_KEYS: AtomicUsize = AtomicUsize::new(0);
    static GAME_KEYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_KEYUP && w == 87 && std::env::args().any(|a| a == "--reentrant-register") {
            let r =
                RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_NOLEGACY | RIDEV_NOHOTKEYS, hwndTarget: h };
            // SAFETY: game callback makes a real registration while Underwolf releases its held key.
            assert_ne!(unsafe { RegisterRawInputDevices(&r, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }, 0);
        }
        if m == WM_KEYUP && w == 87 && std::env::args().any(|a| a == "--reentrant-cursor") {
            let clip = windows_sys::Win32::Foundation::RECT { left: 70, top: 80, right: 300, bottom: 320 };
            // SAFETY: real game cursor requests during its held-release callback.
            unsafe {
                assert_ne!(ClipCursor(&clip), 0);
                SetCursor(std::ptr::null_mut());
                ShowCursor(0);
            }
        }
        if m == WM_KEYUP && w == 87 && std::env::args().any(|a| a == "--release-focus") {
            // SAFETY: game callback creates another window and transfers foreground before returning.
            unsafe {
                let other = CreateWindowExW(
                    0,
                    w!("UnderwolfInputProbe"),
                    w!("Cleanup focus target"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    90,
                    90,
                    300,
                    300,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                );
                assert!(!other.is_null());
                SetForegroundWindow(other);
                SetFocus(other);
            }
        }
        if m == WM_INPUT {
            let mut size = 0u32;
            // SAFETY: message provides an HRAWINPUT; null buffer is the documented size query.
            let query = unsafe {
                read_raw_data(
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
                    read_raw_data(
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
        if raw.len() >= 40
            && u32::from_le_bytes(raw[..4].try_into().unwrap()) == 1
            && u16::from_le_bytes(raw[30..32].try_into().unwrap()) == 87
        {
            if u16::from_le_bytes(raw[26..28].try_into().unwrap()) & 1 == 0 {
                RAW_W_DOWN.fetch_add(1, Ordering::Relaxed);
            } else {
                RAW_W_UP.fetch_add(1, Ordering::Relaxed);
            }
        }
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
            read_raw_buffer(bytes.as_mut_ptr().cast(), &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32)
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
                style: CS_DBLCLKS,
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
        let startup_ctrl = std::env::args().any(|a| a == "--startup-ctrl");
        if startup_ctrl {
            key(162, true);
            for _ in 0..4 {
                pump();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let runtime = InputRuntime::install(hwnd).expect("install");
        let h = runtime.handle();
        h.connect();
        h.set_visible(true);
        if std::env::args().any(|a| a == "--expired-entry") {
            let started = Instant::now();
            let mut epoch = 0;
            let mut requested = false;
            while started.elapsed() < Duration::from_secs(3) {
                pump();
                for event in h.drain() {
                    match event {
                        RuntimeEvent::State(s) => {
                            epoch = s.epoch;
                            assert!(!s.active);
                        }
                        RuntimeEvent::Result(r) if r.request_id == 88 => {
                            assert_eq!(r.code, CaptureCode::Unavailable);
                            key(162, true);
                            key(9, true);
                            for _ in 0..20 {
                                pump();
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            assert!(!h.capture_permitted(), "expired lease accepted a hotkey");
                            key(9, false);
                            key(162, false);
                            println!("PASS input probe: idle expired lease rejects request and hotkey");
                            return;
                        }
                        _ => {}
                    }
                }
                if let Some(c) = h.challenge()
                    && started.elapsed() < Duration::from_millis(200)
                {
                    h.acknowledge(InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq });
                }
                if !requested && started.elapsed() > Duration::from_millis(900) {
                    h.request(CaptureRequest { epoch, state_seq: 88, request_id: 88, on: true }).unwrap();
                    requested = true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("expired entry did not complete");
        }
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
        let double_click = std::env::args().any(|a| a == "--double-click");
        let expected_buttons = [if double_click { 2 } else { 1 }; 5];
        let mut host_wheels = [0; 2];
        let mouse_test = std::env::args().any(|a| a == "--mouse");
        let mut before_mouse = 0;
        let mut last_ack = Instant::now();
        let mut stalled = false;
        let mut released = false;
        let mut before_sent = false;
        let mut startup_released = false;
        let mut before_raw = 0;
        let cursor_test = std::env::args().any(|a| a == "--cursor" || a == "--reentrant-cursor");
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
                    RuntimeEvent::Result(r)
                        if std::env::args().any(|a| {
                            matches!(a.as_str(), "--reentrant-register" | "--reentrant-cursor" | "--release-focus")
                        }) =>
                    {
                        let focus = std::env::args().any(|a| a == "--release-focus");
                        assert_eq!(r.code, if focus { CaptureCode::Superseded } else { CaptureCode::SetupFailed });
                        assert!(!h.capture_permitted());
                        if focus {
                            for _ in 0..20 {
                                pump();
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            assert_eq!(
                                RAW_W_UP.load(Ordering::Relaxed),
                                0,
                                "cleanup injected after callback changed foreground"
                            );
                            println!("PASS input probe: callback focus loss prevents synthetic releases");
                            return;
                        }
                        if std::env::args().any(|a| a == "--reentrant-cursor") {
                            let mut clip = windows_sys::Win32::Foundation::RECT::default();
                            // SAFETY: inspect original game intent after failed entry rollback.
                            unsafe {
                                assert_ne!(GetClipCursor(&mut clip), 0);
                                assert_eq!((clip.left, clip.top, clip.right, clip.bottom), (70, 80, 300, 320));
                                assert!(GetCursor().is_null());
                                let count = ShowCursor(1) - 1;
                                ShowCursor(0);
                                assert_eq!(count, baseline_count - 1);
                            }
                            println!(
                                "PASS input probe: reentrant cursor changes cancel entry and preserve game intent"
                            );
                            return;
                        }
                        let mut count = 16;
                        let mut devices = [RAWINPUTDEVICE::default(); 16];
                        // SAFETY: writable native descriptor buffer and matching count/size.
                        let n = unsafe {
                            GetRegisteredRawInputDevices(
                                devices.as_mut_ptr(),
                                &mut count,
                                std::mem::size_of::<RAWINPUTDEVICE>() as u32,
                            )
                        };
                        assert_ne!(n, u32::MAX);
                        let keyboard =
                            devices[..n as usize].iter().find(|d| d.usUsagePage == 1 && d.usUsage == 6).unwrap();
                        assert_eq!(keyboard.dwFlags, RIDEV_NOLEGACY | RIDEV_NOHOTKEYS);
                        println!("PASS input probe: reentrant game registration cancels entry and preserves intent");
                        return;
                    }
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
            if startup_ctrl && !startup_released && started.elapsed() > Duration::from_millis(100) {
                key(162, false);
                startup_released = true;
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
                if startup_ctrl {
                    key(9, true);
                    key(9, false);
                    for _ in 0..4 {
                        pump();
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(!h.capture_permitted(), "Tab toggled capture after startup Ctrl was released");
                }
                before_raw = RAW_KEYS.load(Ordering::Relaxed);
                before_mouse = RAW_MOUSE.load(Ordering::Relaxed);
                if mouse_test {
                    assert!(before_mouse > 0, "no real raw mouse baseline");
                }
                assert!(before_raw > 0, "probe saw no real raw input before capture");
                if std::env::args().any(|a| a == "--buffered") {
                    assert!(BUFFER_KEYS.load(Ordering::Relaxed) > 0, "buffered probe never observed real input");
                }
                if std::env::args().any(|a| {
                    matches!(a.as_str(), "--held" | "--reentrant-register" | "--reentrant-cursor" | "--release-focus")
                }) {
                    key(87, true);
                    for _ in 0..4 {
                        pump();
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert_eq!(RAW_W_DOWN.load(Ordering::Relaxed), 1);
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
                        let shape = LoadCursorW(std::ptr::null_mut(), IDC_CROSS);
                        assert!(!shape.is_null());
                        SetCursor(shape);
                        assert_eq!(GetCursor(), shape, "game cursor shape query must expose intended shape");
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
                        if double_click {
                            mouse(down, data, 0, 0);
                            mouse(up, data, 0, 0);
                        }
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
                && (!mouse_test || (host_buttons == expected_buttons && host_wheels == [1; 2]))
            {
                if mouse_test && !stalled {
                    assert_eq!(RAW_MOUSE.load(Ordering::Relaxed), before_mouse, "captured raw mouse leaked");
                }
                stalled = true;
                if std::env::args().any(|a| a == "--hide-stall") {
                    let worker = h.clone();
                    let visibility_only = std::env::args().any(|a| a == "--visibility-only");
                    std::thread::spawn(move || {
                        if visibility_only {
                            worker.set_visible_state(false, 77);
                        } else {
                            worker.apply_capture(
                                CaptureRequest { epoch, state_seq: 77, request_id: 77, on: false },
                                false,
                                77,
                            );
                        }
                        assert!(!worker.capture_permitted(), "hide did not immediately revoke permission");
                        let started = Instant::now();
                        while started.elapsed() < Duration::from_millis(250) {
                            for event in worker.drain() {
                                assert!(
                                    !matches!(event, RuntimeEvent::Applied(77)),
                                    "hide applied before UI restoration"
                                );
                            }
                            if let Some(c) = worker.challenge() {
                                worker.acknowledge(InputAck {
                                    epoch: c.epoch,
                                    nonce: c.nonce,
                                    through_seq: c.through_seq,
                                });
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    })
                    .join()
                    .unwrap();
                    let resumed = Instant::now();
                    while resumed.elapsed() < Duration::from_secs(2) {
                        pump();
                        for event in h.drain() {
                            if matches!(event, RuntimeEvent::Applied(77)) {
                                assert!(!h.capture_permitted());
                                println!(
                                    "PASS input probe: hide revokes before stalled UI and acknowledges after restoration"
                                );
                                return;
                            }
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    panic!("hide never acknowledged after UI resumed");
                }
                if std::env::args().any(|a| a == "--focus") {
                    // SAFETY: create another probe-owned window and transfer actual OS focus.
                    let other = unsafe {
                        let other = CreateWindowExW(
                            0,
                            w!("UnderwolfInputProbe"),
                            w!("Other foreground"),
                            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                            80,
                            80,
                            640,
                            480,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            GetModuleHandleW(std::ptr::null()),
                            std::ptr::null(),
                        );
                        assert!(!other.is_null());
                        SetForegroundWindow(other);
                        SetFocus(other);
                        other
                    };
                    for _ in 0..20 {
                        pump();
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(!h.capture_permitted());
                    // SAFETY: focus returns to the probe, but this is not a capture request.
                    unsafe {
                        SetForegroundWindow(hwnd);
                        SetFocus(hwnd);
                        DestroyWindow(other);
                    }
                    for _ in 0..20 {
                        pump();
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(!h.capture_permitted(), "focus return recaptured automatically");
                    println!("PASS input probe: real focus transfer releases and return does not recapture");
                    return;
                }
                if std::env::args().any(|a| a == "--replace" || a == "--replace-pending") {
                    if std::env::args().any(|a| a == "--replace-pending") {
                        for _ in 0..10000 {
                            // SAFETY: game-owned count changes force a multi-wake restoration.
                            unsafe {
                                ShowCursor(1);
                            }
                        }
                    }
                    // SAFETY: replace the probe-owned window on its UI thread.
                    let replacement = unsafe {
                        DestroyWindow(hwnd);
                        let next = CreateWindowExW(
                            0,
                            w!("UnderwolfInputProbe"),
                            w!("Replacement probe"),
                            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                            80,
                            80,
                            640,
                            480,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            GetModuleHandleW(std::ptr::null()),
                            std::ptr::null(),
                        );
                        assert!(!next.is_null());
                        SetForegroundWindow(next);
                        SetFocus(next);
                        next
                    };
                    let replacement_start = Instant::now();
                    let mut replacement_epoch = 0;
                    let mut replacement_requested = false;
                    while replacement_start.elapsed() < Duration::from_secs(3) {
                        pump();
                        for event in h.drain() {
                            eprintln!("replacement {event:?}");
                            match event {
                                RuntimeEvent::State(s) => replacement_epoch = s.epoch,
                                RuntimeEvent::Result(r) if r.request_id == 99 => {
                                    assert_eq!(r.code, CaptureCode::Applied);
                                    assert!(h.capture_permitted());
                                    // SAFETY: destroy only the replacement window owned by this probe.
                                    unsafe {
                                        DestroyWindow(replacement);
                                    }
                                    println!("PASS input probe: replacement window rebinds without automatic capture");
                                    return;
                                }
                                _ => {}
                            }
                        }
                        if let Some(c) = h.challenge() {
                            h.acknowledge(InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq });
                        }
                        if !replacement_requested && replacement_start.elapsed() > Duration::from_millis(600) {
                            assert!(!h.capture_permitted(), "replacement automatically recaptured");
                            h.request(CaptureRequest {
                                epoch: replacement_epoch,
                                state_seq: 99,
                                request_id: 99,
                                on: true,
                            })
                            .unwrap();
                            replacement_requested = true;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    panic!("replacement never became available");
                }
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
            assert_eq!(host_buttons, expected_buttons, "five buttons exactly once");
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
        if std::env::args().any(|a| a == "--held") {
            assert_eq!(RAW_W_UP.load(Ordering::Relaxed), 1, "game did not receive one synthetic release");
            key(87, true);
            for _ in 0..4 {
                pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(RAW_W_DOWN.load(Ordering::Relaxed), 1, "held key repeated into game after exit");
            key(87, false);
            for _ in 0..4 {
                pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            key(87, true);
            for _ in 0..4 {
                pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(RAW_W_DOWN.load(Ordering::Relaxed), 2, "fresh key press did not resume");
            key(87, false);
        }
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
                let shape = LoadCursorW(std::ptr::null_mut(), IDC_CROSS);
                assert_eq!(GetCursor(), shape, "latest game cursor shape was not restored");
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
