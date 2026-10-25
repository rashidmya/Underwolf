//! Independent SDK declarations; no adapter ABI types or simulator objects.
use super::drive;
use std::ffi::c_void;
use uw_input::win::{InputHandle, RuntimeEvent};
use uw_pipe_proto::routing::*;
use windows_sys::{
    Win32::{
        Foundation::{HWND, POINT},
        Graphics::Gdi::ClientToScreen,
        System::LibraryLoader::*,
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::{GUID, w},
};
fn key(down: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wScan: 0x1e,
                dwFlags: KEYEVENTF_SCANCODE | if down { 0 } else { KEYEVENTF_KEYUP },
                ..Default::default()
            },
        },
    };
    // SAFETY: initialized scan-code input; DirectInput 8 intentionally does not infer a missing scan code.
    assert_eq!(unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) }, 1);
}
static MODIFIER_EDGES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
unsafe extern "system" fn observe_keyboard(code: i32, w: usize, l: isize) -> isize {
    if code == HC_ACTION as i32 {
        // SAFETY: native callback owns this keyboard structure through callback return.
        let key = unsafe { &*(l as *const KBDLLHOOKSTRUCT) };
        if matches!(key.vkCode, 18 | 164 | 165) {
            MODIFIER_EDGES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
    // SAFETY: the observer never consumes or changes an input event.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}
struct Observer(HHOOK);
impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: this fixture owns this observer and its callback remains resident.
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
    }
}
static OBSERVED_POINT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
unsafe extern "system" fn observe_mouse(code: i32, w: usize, l: isize) -> isize {
    if code == HC_ACTION as i32 {
        // SAFETY: native low-level callback supplies this structure until return.
        let point = unsafe { &*(l as *const MSLLHOOKSTRUCT) }.pt;
        OBSERVED_POINT
            .store((point.x as u32 as u64) | ((point.y as u32 as u64) << 32), std::sync::atomic::Ordering::Release);
    }
    // SAFETY: the independent observer forwards every original argument and consumes nothing.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
}
type Raw = *mut c_void;
type Ref = unsafe extern "system" fn(Raw) -> u32;
type Change = unsafe extern "system" fn(Raw) -> i32;
type State = unsafe extern "system" fn(Raw, u32, Raw) -> i32;
type Data = unsafe extern "system" fn(Raw, u32, Raw, *mut u32, u32) -> i32;
const NOT_ACQUIRED: i32 = 0x8007_000cu32 as i32;
#[repr(C)]
struct Object {
    guid: *const GUID,
    offset: u32,
    kind: u32,
    flags: u32,
}
#[repr(C)]
struct Format {
    size: u32,
    object_size: u32,
    flags: u32,
    data_size: u32,
    count: u32,
    objects: *const Object,
}
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Datum {
    offset: u32,
    data: u32,
    timestamp: u32,
    sequence: u32,
    application: usize,
}
unsafe fn slot(p: Raw, index: usize) -> usize {
    // SAFETY: caller owns the independently selected SDK interface and supplies its valid method slot.
    unsafe { *(*(p.cast::<*const usize>())).add(index) }
}
struct Reference(Raw);
impl Drop for Reference {
    fn drop(&mut self) {
        // SAFETY: SDK IUnknown prefix on a reference owned by this fixture.
        unsafe {
            std::mem::transmute::<usize, Ref>(slot(self.0, 2))(self.0);
        }
    }
}
struct Device {
    reference: Reference,
    mouse: bool,
    size: u32,
}
impl Device {
    fn refcounts(&self) -> (u32, u32) {
        // SAFETY: balanced native AddRef/Release on a reference owned by this fixture.
        unsafe {
            (
                std::mem::transmute::<usize, Ref>(slot(self.reference.0, 1))(self.reference.0),
                std::mem::transmute::<usize, Ref>(slot(self.reference.0, 2))(self.reference.0),
            )
        }
    }
    fn alias_w(&self) -> Self {
        let iid = GUID::from_u128(0x54d41081_dc15_4833_a41b_748f73a38179);
        let mut output = std::ptr::null_mut();
        assert_eq!(
            // SAFETY: real SDK QueryInterface with the independently declared Device8W IID.
            unsafe {
                std::mem::transmute::<usize, unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32>(slot(
                    self.reference.0,
                    0,
                ))(self.reference.0, &iid, &mut output)
            },
            0
        );
        Self { reference: Reference(output), mouse: self.mouse, size: self.size }
    }
    fn change(&self, method: usize) -> i32 {
        // SAFETY: valid owned device; the fixture uses only Acquire and Unacquire slots.
        unsafe { std::mem::transmute::<usize, Change>(slot(self.reference.0, method))(self.reference.0) }
    }
    fn state(&self, size: u32) -> (i32, [u8; 16]) {
        let mut bytes = [0xcc; 16];
        assert!(size <= bytes.len() as u32);
        // SAFETY: owned device, correct SDK state signature and bounded writable fixture buffer.
        let hr = unsafe {
            std::mem::transmute::<usize, State>(slot(self.reference.0, 9))(
                self.reference.0,
                size,
                bytes.as_mut_ptr().cast(),
            )
        };
        (hr, bytes)
    }
    fn data(&self, flags: u32) -> (i32, Vec<Datum>) {
        let mut records = [Datum::default(); 16];
        let mut count = records.len() as u32;
        // SAFETY: initialized SDK data array and valid count; flags are 0 or DIGDD_PEEK.
        let hr = unsafe {
            std::mem::transmute::<usize, Data>(slot(self.reference.0, 10))(
                self.reference.0,
                std::mem::size_of::<Datum>() as u32,
                records.as_mut_ptr().cast(),
                &mut count,
                flags,
            )
        };
        if hr < 0 {
            return (hr, vec![]);
        }
        assert!(count <= records.len() as u32);
        (hr, records[..count as usize].to_vec())
    }
    fn data_shape(&self, size: u32, mut count: u32) -> (i32, u32) {
        let mut records = [Datum::default(); 16];
        assert!(count <= 16 && size <= std::mem::size_of::<Datum>() as u32);
        // SAFETY: bounded storage and count, including a deliberately invalid native record size.
        let hr = unsafe {
            std::mem::transmute::<usize, Data>(slot(self.reference.0, 10))(
                self.reference.0,
                size,
                records.as_mut_ptr().cast(),
                &mut count,
                0,
            )
        };
        (hr, count)
    }
    fn buffer_size(&self, length: u32) -> i32 {
        let property = [20u32, 16, 0, 0, length];
        // SAFETY: documented DIPROP_BUFFERSIZE pseudo-GUID and complete native DIPROPDWORD.
        unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(Raw, *const GUID, *const u32) -> i32>(slot(
                self.reference.0,
                6,
            ))(self.reference.0, std::ptr::without_provenance::<GUID>(1), property.as_ptr())
        }
    }
    fn flush(&self) -> i32 {
        let mut count = u32::MAX;
        // SAFETY: SDK's documented NULL/INFINITE flush, using a real owned device.
        unsafe {
            std::mem::transmute::<usize, Data>(slot(self.reference.0, 10))(
                self.reference.0,
                std::mem::size_of::<Datum>() as u32,
                std::ptr::null_mut(),
                &mut count,
                0,
            )
        }
    }
}
pub(super) struct Fixture {
    factories: Vec<Reference>,
    devices: Vec<Device>,
    hwnd: HWND,
    counts: Vec<(u32, u32)>,
    _observer: Observer,
}
impl Fixture {
    pub fn create(hwnd: HWND) -> Self {
        // SAFETY: the fixture alone loads the native system runtime.
        let module = unsafe { LoadLibraryW(w!("dinput8.dll")) };
        assert!(!module.is_null());
        // SAFETY: exact SDK factory export/signature, independently declared.
        let create = unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(Raw, u32, *const GUID, *mut Raw, Raw) -> i32,
            >(GetProcAddress(module, c"DirectInput8Create".as_ptr().cast()).unwrap())
        };
        // SAFETY: install before the production fallback so this sees only forwarded native edges.
        let observer =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(observe_keyboard), GetModuleHandleW(std::ptr::null()), 0) };
        assert!(!observer.is_null());
        let mut fixture =
            Self { factories: vec![], devices: vec![], hwnd, counts: vec![], _observer: Observer(observer) };
        for iid in [
            GUID::from_u128(0xbf798030_483a_4da2_aa99_5d64ed369700),
            GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700),
        ] {
            let mut pointer = std::ptr::null_mut();
            assert_eq!(
                // SAFETY: documented factory inputs and writable owned output.
                unsafe { create(GetModuleHandleW(std::ptr::null()), 0x800, &iid, &mut pointer, std::ptr::null_mut()) },
                0
            );
            fixture.factories.push(Reference(pointer));
        }
        for factory in &fixture.factories {
            for mouse in [false, true] {
                fixture.devices.push(Self::device(factory, hwnd, mouse));
            }
        }
        for device in &fixture.devices {
            assert!(device.change(7) >= 0);
            assert_eq!(device.state(device.size).0, 0, "native pre-install read");
        }
        key(true);
        for _ in 0..10 {
            super::pump();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for device in fixture.devices.iter().filter(|d| !d.mouse) {
            let (hr, bytes) = device.state(device.size);
            assert_eq!(hr, 0);
            assert_eq!(bytes[3], 0x80, "native pre-install custom keyboard mapping");
            assert!(!device.data(1).1.is_empty(), "native pre-install buffered keyboard path");
            assert!(device.flush() >= 0);
        }
        key(false);
        for _ in 0..10 {
            super::pump();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fixture.counts = fixture.devices.iter().map(Device::refcounts).collect();
        fixture
    }
    fn device(factory: &Reference, hwnd: HWND, mouse: bool) -> Device {
        let guid = GUID::from_u128(if mouse {
            0x6f1d2b60_d5a0_11cf_bfc7_444553540000
        } else {
            0x6f1d2b61_d5a0_11cf_bfc7_444553540000
        });
        let mut pointer = std::ptr::null_mut();
        assert_eq!(
            // SAFETY: real factory with a valid native CreateDevice method and keyboard/mouse GUID.
            unsafe {
                std::mem::transmute::<usize, unsafe extern "system" fn(Raw, *const GUID, *mut Raw, Raw) -> i32>(slot(
                    factory.0, 3,
                ))(factory.0, &guid, &mut pointer, std::ptr::null_mut())
            },
            0
        );
        let device = Device { reference: Reference(pointer), mouse, size: if mouse { 8 } else { 4 } };
        let object_guid = GUID::from_u128(if mouse {
            0xa36d02e0_c9f3_11cf_bfc7_444553540000
        } else {
            0x55728220_d33c_11cf_bfc7_444553540000
        });
        let object = Object {
            guid: &object_guid,
            offset: if mouse { 4 } else { 3 },
            kind: if mouse { 0x8000_0001 } else { 0x8000_1e0c },
            flags: 0,
        };
        let format = Format {
            size: std::mem::size_of::<Format>() as u32,
            object_size: std::mem::size_of::<Object>() as u32,
            flags: if mouse { 2 } else { 0 },
            data_size: device.size,
            count: 1,
            objects: &object,
        };
        // SAFETY: exact SDK format and cooperative-level methods, valid fixture-owned inputs.
        unsafe {
            assert_eq!(
                std::mem::transmute::<usize, unsafe extern "system" fn(Raw, *const Format) -> i32>(slot(pointer, 11))(
                    pointer, &format
                ),
                0
            );
            let cooperative = if std::env::args().any(|a| a == "directinput-exclusive") { 5 } else { 6 };
            assert_eq!(
                std::mem::transmute::<usize, unsafe extern "system" fn(Raw, HWND, u32) -> i32>(slot(pointer, 13))(
                    pointer,
                    hwnd,
                    cooperative
                ),
                0
            );
            let buffer = [20u32, 16, 0, 0, 4];
            assert_eq!(
                std::mem::transmute::<usize, unsafe extern "system" fn(Raw, *const GUID, *const u32) -> i32>(slot(
                    pointer, 6
                ))(pointer, std::ptr::without_provenance::<GUID>(1), buffer.as_ptr()),
                0
            );
        }
        device
    }
    fn request(h: &InputHandle, epoch: u64, on: bool, id: u32) {
        h.route_request(RoutingRequest {
            epoch,
            revision: 1,
            request_id: id,
            target: None,
            action: if on { RoutingAction::EnterEdit } else { RoutingAction::LeaveEdit },
            state_seq: None,
        })
        .unwrap();
        let events = drive(h, 80);
        assert!(
            events.iter().any(
                |e| matches!(e, RuntimeEvent::RoutingResult(r) if r.request_id == id && r.code == RoutingCode::Applied)
            ),
            "native routing request {id}: {events:?}"
        );
    }
    pub fn exercise(mut self, h: &InputHandle, epoch: u64, mut coverage: Vec<RuntimeEvent>) {
        coverage.extend(drive(h, 300));
        assert!(coverage.iter().any(|e| matches!(e, RuntimeEvent::NativeCoverage(c) if c.directinput.status == NativeStatus::Supported && c.directinput.abi_mask == 3)), "DirectInput isolation was not installed: {coverage:?}");
        assert_eq!(
            self.devices.iter().map(Device::refcounts).collect::<Vec<_>>(),
            self.counts,
            "adapter changed native object reference counts"
        );
        if std::env::args().any(|a| a == "directinput-exclusive") {
            super::key_vk(113, true);
            let mut events = drive(h, 40);
            super::key_vk(113, false);
            events.extend(drive(h, 40));
            assert_eq!(
                events
                    .iter()
                    .filter_map(|e| if let RuntimeEvent::RoutedBatch(b) = e {
                        Some(b.events.iter().filter(|e| e.kind == EventKind::Hotkey && e.argument == 2).count())
                    } else {
                        None
                    })
                    .sum::<usize>(),
                1,
                "exclusive DirectInput hid the named hotkey"
            );
        }
        // Devices from a factory created before installation, but first created after hooks.
        for factory in &self.factories {
            let d = Self::device(factory, self.hwnd, false);
            assert!(d.change(7) >= 0);
            self.devices.push(d);
        }
        for device in &self.devices {
            assert_eq!(
                device.data_shape(std::mem::size_of::<Datum>() as u32, 0),
                (0, 0),
                "zero-count native read changed"
            );
            assert!(device.data_shape(1, 16).0 < 0, "invalid native record size succeeded");
        }
        let unbuffered = Self::device(&self.factories[0], self.hwnd, false);
        assert!(unbuffered.buffer_size(0) >= 0);
        assert!(unbuffered.change(7) >= 0);
        let not_buffered = unbuffered.data(0).0;
        assert!(not_buffered < 0);
        let failures: Vec<_> = self.devices.iter().map(|d| d.state(d.size + 1).0).collect();
        assert!(failures.iter().all(|hr| *hr < 0));
        for _ in 0..6 {
            key(true);
            drive(h, 10);
            key(false);
            drive(h, 10);
        }
        for d in self.devices.iter().filter(|d| !d.mouse) {
            let first = d.data(1);
            let second = d.data(1);
            assert_eq!(first.0, 1, "real overflow HRESULT was lost");
            assert!(!first.1.is_empty(), "post-install keyboard data: {:?}, state {:?}", first, d.state(d.size));
            assert_eq!(first.1, second.1, "PEEK consumed data");
            assert!(d.flush() >= 0);
            assert!(d.data(0).1.is_empty());
        }
        self.devices.push(self.devices[0].alias_w());
        Self::request(h, epoch, true, 30);
        MODIFIER_EDGES.store(0, std::sync::atomic::Ordering::Release);
        super::key_vk(164, true);
        drive(h, 30);
        // SAFETY: game-facing polling stays filtered even when the desktop receives modifier edges.
        assert_eq!(unsafe { GetAsyncKeyState(164) } & i16::MIN, 0);
        super::key_vk(164, false);
        drive(h, 30);
        assert_eq!(
            MODIFIER_EDGES.load(std::sync::atomic::Ordering::Acquire),
            2,
            "fallback prevented desktop modifier tracking"
        );
        assert_eq!(unbuffered.data(0).0, not_buffered, "capture replaced an original unbuffered failure");
        for device in &self.devices {
            assert_eq!(device.data_shape(1, 16).0, NOT_ACQUIRED);
            assert_eq!(device.data_shape(std::mem::size_of::<Datum>() as u32, 0).0, NOT_ACQUIRED);
        }
        drop(unbuffered);
        // No device API is polled during this interaction: exclusive drivers cannot hide overlay input.
        // SAFETY: fixture client point and initialized absolute desktop mouse movement.
        unsafe {
            let mut point = POINT { x: 20, y: 20 };
            ClientToScreen(self.hwnd, &mut point);
            let input = INPUT {
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
            assert_eq!(SendInput(1, &input, std::mem::size_of::<INPUT>() as i32), 1);
        }
        drive(h, 30);
        super::click(true);
        let mut interaction = drive(h, 30);
        super::click(false);
        interaction.extend(drive(h, 30));
        assert!(
            interaction.iter().any(
                |e| matches!(e, RuntimeEvent::RoutingState(s) if s.focus == Some(WidgetRef { id: 1, generation: 1 }))
            ),
            "exclusive input hid the widget click: {interaction:?}"
        );
        key(true);
        interaction.extend(drive(h, 30));
        key(false);
        interaction.extend(drive(h, 30));
        assert!(interaction.iter().any(|e| matches!(e, RuntimeEvent::RoutedBatch(b) if b.events.iter().any(|e| e.input.is_some_and(|r| r.kind == 2 && r.a == 97)))), "fallback text was lost");
        for d in &self.devices {
            assert_eq!(d.state(d.size).0, NOT_ACQUIRED);
            assert_eq!(d.data(1).0, NOT_ACQUIRED);
            assert_eq!(d.change(7), NOT_ACQUIRED);
        }
        key(true);
        drive(h, 30);
        Self::request(h, epoch, false, 31);
        for d in self.devices.iter().filter(|d| !d.mouse) {
            assert_eq!(d.state(d.size).0, NOT_ACQUIRED, "held key escaped reacquisition quarantine");
        }
        key(false);
        drive(h, 40);
        for d in &self.devices {
            assert!(d.data(0).1.is_empty(), "captured buffered input replayed");
        }
        key(true);
        drive(h, 30);
        for d in self.devices.iter().filter(|d| !d.mouse) {
            let (hr, state) = d.state(d.size);
            assert_eq!(hr, 0, "a fresh post-capture press was mistaken for captured-held quarantine");
            assert_eq!(state[3], 0x80);
        }
        key(false);
        drive(h, 30);
        for d in self.devices.iter().take(self.devices.len() - 1).filter(|d| !d.mouse) {
            let fresh = d.data(0);
            assert_eq!(
                fresh.1.iter().map(|r| r.data).collect::<Vec<_>>(),
                vec![0x80, 0],
                "fresh post-capture buffered input was lost"
            );
        }
        for (d, failure) in self.devices.iter().zip(&failures) {
            assert_eq!(d.state(d.size).0, 0);
            assert_eq!(d.state(d.size + 1).0, *failure);
            assert!(d.data(0).1.is_empty(), "captured buffered input replayed");
        }
        Self::request(h, epoch, true, 32);
        assert!(self.devices[0].change(8) >= 0);
        Self::request(h, epoch, false, 33);
        assert_eq!(self.devices[0].state(self.devices[0].size).0, NOT_ACQUIRED, "explicit Unacquire intent was lost");
        assert_eq!(
            self.devices.last().unwrap().state(self.devices.last().unwrap().size).0,
            NOT_ACQUIRED,
            "QI alias lost its shared acquisition intent"
        );
        assert_eq!(self.devices[2].state(self.devices[2].size).0, 0);
        for d in &self.devices {
            assert!(d.change(7) >= 0);
            assert!(d.flush() >= 0);
        }
        key(true);
        drive(h, 30);
        Self::request(h, epoch, true, 34);
        key(false);
        drive(h, 30);
        Self::request(h, epoch, false, 35);
        for d in self.devices.iter().filter(|d| !d.mouse) {
            assert!(d.data(0).1.is_empty(), "an unpolled capture interval replayed old input");
        }
        // Concurrent last releases must retire the native registry before any UI borrow.
        for round in 0..64 {
            let device = Self::device(&self.factories[0], self.hwnd, false);
            let alias = device.alias_w();
            let first = device.reference.0 as usize;
            let second = alias.reference.0 as usize;
            std::mem::forget(device);
            std::mem::forget(alias);
            let barrier = std::sync::Barrier::new(3);
            std::thread::scope(|scope| {
                for pointer in [first, second] {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        // SAFETY: each thread receives exactly one real owned native reference.
                        unsafe {
                            std::mem::transmute::<usize, Ref>(slot(pointer as Raw, 2))(pointer as Raw);
                        }
                    });
                }
                barrier.wait();
            });
            Self::request(h, epoch, true, 100 + round * 2);
            Self::request(h, epoch, false, 101 + round * 2);
        }
        // The same low-level callbacks must pass desktop input after foreground loss.
        let before = super::GAME_KEYS.load(std::sync::atomic::Ordering::Acquire);
        // SAFETY: create and focus only another window in this private fixture process.
        let other = unsafe {
            let other = CreateWindowExW(
                0,
                w!("UnderwolfRoutingProbe"),
                w!("Routing foreground fixture"),
                WS_OVERLAPPEDWINDOW,
                200,
                200,
                300,
                200,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            assert!(!other.is_null());
            ShowWindow(other, SW_SHOWNOACTIVATE);
            SetForegroundWindow(self.hwnd);
            SetWindowPos(other, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            other
        };
        drive(h, 30);
        Self::request(h, epoch, true, 36);
        // SAFETY: independent observer sees native physical coordinates before the adapter.
        let observer =
            unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(observe_mouse), GetModuleHandleW(std::ptr::null()), 0) };
        assert!(!observer.is_null());
        // SAFETY: move to another actual desktop window without first activating it by API.
        unsafe {
            assert_eq!(GetForegroundWindow(), self.hwnd);
            let mut point = POINT { x: 20, y: 20 };
            ClientToScreen(other, &mut point);
            let input = INPUT {
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
            assert_eq!(SendInput(1, &input, std::mem::size_of::<INPUT>() as i32), 1);
        }
        drive(h, 30);
        // SAFETY: assert the physical pointer really reached the fixture's desktop target.
        unsafe {
            let observed = OBSERVED_POINT.load(std::sync::atomic::Ordering::Acquire);
            let actual = POINT { x: observed as u32 as i32, y: (observed >> 32) as u32 as i32 };
            assert_eq!(
                GetAncestor(WindowFromPhysicalPoint(actual), GA_ROOT),
                other,
                "pointer did not reach desktop fixture"
            );
        }
        super::CLICK_TARGET.store(0, std::sync::atomic::Ordering::Release);
        super::click(true);
        drive(h, 30);
        super::click(false);
        drive(h, 30);
        // SAFETY: retire only this fixture's observer after the click.
        unsafe {
            UnhookWindowsHookEx(observer);
        }
        // SAFETY: query the real foreground after the first physical desktop click.
        assert_eq!(
            super::CLICK_TARGET.load(std::sync::atomic::Ordering::Acquire),
            other as usize,
            "fallback swallowed the first desktop click"
        );
        // SAFETY: Wine without a window manager need not activate on click; explicitly test keyboard focus.
        unsafe {
            SetForegroundWindow(other);
            SetFocus(other);
        }
        drive(h, 30);
        key(true);
        drive(h, 40);
        assert!(
            super::GAME_KEYS.load(std::sync::atomic::Ordering::Acquire) > before,
            "desktop key was swallowed after focus loss"
        );
        // SAFETY: the real export remains game-visible outside ownership.
        assert_ne!(unsafe { GetAsyncKeyState(65) } & i16::MIN, 0);
        key(false);
        drive(h, 30);
        // SAFETY: destroy only the fixture's secondary window.
        unsafe {
            DestroyWindow(other);
        }
        // Final Release runs through actual COM lifetime hooks; no retained adapter references.
        drop(self);
    }
}
