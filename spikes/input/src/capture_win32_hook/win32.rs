//! The in-process Win32 hook that drives the tested core on Windows/Proton (candidate C3).
//!
//! This module is `#[cfg(windows)]` and is compiled into the injectable PE DLL with
//! `--target x86_64-pc-windows-gnu`. It is **not** exercised by the Linux CI build or the
//! integration tests; the owner verifies it on hardware against the Task 7 P1–P6 matrix. All of
//! its decision logic lives in the parent module — [`handle_message`](super::handle_message) for
//! keyboard and [`handle_raw_mouse`](super::handle_raw_mouse) /
//! [`neutralize_raw_mouse`](super::neutralize_raw_mouse) for the mouse — and those are unit-tested.
//!
//! Mechanism:
//! - **Keyboard** goes through a `WndProc` subclass: every key message passes through the gate,
//!   which detects Ctrl+Tab and swallows game input while editing. Deadlock delivers keyboard as
//!   legacy `WM_KEY*`, so releases synthesized on edit entry reach it via `CallWindowProcW`.
//! - **Mouse** goes through an IAT hook of `user32!GetRawInputData`. Source 2 reads mouse-look as
//!   raw input, which a `WndProc` subclass never sees (this was C2's mouse leak and C3 review I5).
//!   The hook lets the real call fill the game's buffer, forwards the motion/clicks to the host,
//!   and — while editing — zeroes the buffer in place so the camera does not move (P2).
//! - **Held-button release (P5, "no stuck fire").** On edit entry the gate asks to release any
//!   held mouse button. A raw-input game ignores `WM_*BUTTONUP`, so we `SendInput` a real button-up
//!   tagged with [`SYNTH_EXTRA_INFO`]; the raw hook recognizes that tag and passes it straight to
//!   the game instead of swallowing it.
//!
//! Transport: the native overlay host runs outside the Proton container, so the hook forwards over
//! a localhost TCP socket (Wine's Winsock maps to real host sockets). The host runs a TCP listener
//! for the C3 path (`docs/spikes/01-linux-input.md`).
//!
//! Known gaps for hardware verification: keyboard is assumed legacy `WM_KEY*` (a raw-keyboard game
//! would bypass the WndProc gate); the IAT hook assumes `GetRawInputData` is statically imported
//! from user32 (a dynamically resolved call would need an inline detour instead).

use std::ffi::c_void;
use std::net::TcpStream;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{BOOL, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, EnumWindows, GWLP_WNDPROC, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible,
    SetWindowLongPtrW, WNDPROC,
};

use uw_spike_ipc::{InputEvent, Msg, PROTOCOL, write_msg};

use super::{BTN_MIDDLE, BTN_RIGHT, HostSink, MessageOutcome, handle_message, handle_raw_mouse, wm};
use crate::gate::{GameAction, InputGate};

/// Tag on our synthetic releases so the raw hook lets them reach the game unmolested.
const SYNTH_EXTRA_INFO: u32 = 0x5557_4F4C; // "UWOL"
/// `GetRawInputData(.., RID_INPUT, ..)` copies the RAWINPUT itself (vs. just the header).
const RID_INPUT: u32 = 0x1000_0003;
/// `ulExtraInformation` is the last `ULONG` of the RAWMOUSE on x86_64.
const RAW_EXTRA_INFO_OFF: usize = 44;

struct HookState {
    gate: InputGate,
    host: ChannelHost,
}

static STATE: OnceLock<Mutex<HookState>> = OnceLock::new();
static ORIG_WNDPROC: AtomicIsize = AtomicIsize::new(0);

type GetRawInputDataFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> u32;
static REAL_GET_RAW_INPUT_DATA: AtomicUsize = AtomicUsize::new(0);

type RegisterRawInputDevicesFn = unsafe extern "system" fn(*const c_void, u32, u32) -> BOOL;
static REAL_REGISTER_RAW_INPUT_DEVICES: AtomicUsize = AtomicUsize::new(0);

/// Host sink that hands messages to a background sender thread. It never blocks the game's message
/// pump: a full queue drops events rather than stalling the `WndProc` (Review Focus / P6).
struct ChannelHost {
    tx: SyncSender<Msg>,
}

impl HostSink for ChannelHost {
    fn input(&mut self, event: &InputEvent) {
        let _ = self.tx.try_send(Msg::Input(event.clone()));
    }
    fn edit_mode_changed(&mut self, on: bool) {
        let _ = self.tx.try_send(Msg::EditMode { on });
    }
}

fn is_keyboard_message(msg: u32) -> bool {
    matches!(msg, wm::WM_KEYDOWN | wm::WM_KEYUP | wm::WM_SYSKEYDOWN | wm::WM_SYSKEYUP)
}

/// Deliver a real button-up to the game, tagged so the raw hook passes it through.
fn send_button_up(button: u8) {
    let flags = match button {
        BTN_RIGHT => MOUSEEVENTF_RIGHTUP,
        BTN_MIDDLE => MOUSEEVENTF_MIDDLEUP,
        _ => MOUSEEVENTF_LEFTUP,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: SYNTH_EXTRA_INFO as usize,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let orig: WNDPROC = unsafe { std::mem::transmute(ORIG_WNDPROC.load(Ordering::Acquire)) };
    let call_orig = |m: u32, w: WPARAM, l: LPARAM| -> LRESULT { unsafe { CallWindowProcW(orig, hwnd, m, w, l) } };

    // Decide under the lock but call nothing into Win32 while it is held. Mouse is owned by the
    // raw-input hook; here we only route keyboard through the gate and swallow legacy mouse
    // messages while editing so a non-raw game sees nothing either.
    let outcome = match STATE.get().and_then(|s| s.lock().ok()) {
        Some(mut st) => {
            if is_keyboard_message(msg) {
                let st = &mut *st;
                handle_message(msg, wparam.0, lparam.0, &mut st.gate, &mut st.host)
            } else if st.gate.edit_mode() && super::translate_message(msg, wparam.0, lparam.0).is_some() {
                MessageOutcome { swallow: true, synth: Vec::new() }
            } else {
                MessageOutcome { swallow: false, synth: Vec::new() }
            }
        }
        None => return call_orig(msg, wparam, lparam),
    };

    // Inject synthetic releases so keys/buttons held when edit mode opened do not stick down in the
    // game (Review Focus 3 / P5). Keyboard is legacy WM_*; mouse must go back in as raw input.
    for action in &outcome.synth {
        match action {
            GameAction::SynthKeyUp(code) => {
                call_orig(wm::WM_KEYUP, WPARAM(*code as usize), LPARAM(0));
            }
            GameAction::SynthButtonUp(button) => send_button_up(*button),
            GameAction::Pass(_) => {}
        }
    }

    if outcome.swallow {
        return LRESULT(0);
    }
    call_orig(msg, wparam, lparam)
}

/// IAT hook for `user32!GetRawInputData`. Fills the game's buffer with the real data, forwards and
/// (while editing) neutralizes the mouse, and passes our own tagged releases straight through.
unsafe extern "system" fn hooked_get_raw_input_data(
    h_raw_input: *mut c_void,
    ui_command: u32,
    p_data: *mut c_void,
    pcb_size: *mut u32,
    cb_size_header: u32,
) -> u32 {
    let real: GetRawInputDataFn = unsafe { std::mem::transmute(REAL_GET_RAW_INPUT_DATA.load(Ordering::Acquire)) };
    let ret = unsafe { real(h_raw_input, ui_command, p_data, pcb_size, cb_size_header) };

    if ui_command == RID_INPUT && !p_data.is_null() && ret != u32::MAX && (ret as usize) >= super::ri_min_len() {
        let buf = unsafe { std::slice::from_raw_parts_mut(p_data as *mut u8, ret as usize) };
        if super::raw_mouse_extra_info(buf) == Some(SYNTH_EXTRA_INFO) {
            // Our own synthetic release: strip the tag and let the game have the release unchanged.
            buf[RAW_EXTRA_INFO_OFF..RAW_EXTRA_INFO_OFF + 4].fill(0);
        } else if let Some(mut st) = STATE.get().and_then(|s| s.lock().ok()) {
            let st = &mut *st;
            handle_raw_mouse(buf, &mut st.gate, &mut st.host);
        }
    }
    ret
}

/// IAT hook for `user32!RegisterRawInputDevices`. Transparent for now (the hook point exists so a
/// later version can adjust the game's raw-input registration); forwards to the real call.
unsafe extern "system" fn hooked_register_raw_input_devices(
    p_raw_input_devices: *const c_void,
    num_devices: u32,
    cb_size: u32,
) -> BOOL {
    let real: RegisterRawInputDevicesFn =
        unsafe { std::mem::transmute(REAL_REGISTER_RAW_INPUT_DEVICES.load(Ordering::Acquire)) };
    unsafe { real(p_raw_input_devices, num_devices, cb_size) }
}

// ---- IAT patching ----------------------------------------------------------------------------
//
// Walk the module's import table by raw RVA offsets (robust across `windows`-crate versions), find
// `user32.dll`'s thunk for `func`, make it writable, and swap in `replacement`. Returns the
// original pointer. PE layout per `winnt.h` (PE32+).

unsafe fn read_u32(base: *const u8, off: usize) -> u32 {
    unsafe { (base.add(off) as *const u32).read_unaligned() }
}
unsafe fn read_u64(base: *const u8, off: usize) -> u64 {
    unsafe { (base.add(off) as *const u64).read_unaligned() }
}

unsafe fn cstr_eq_ignore_case(p: *const u8, want: &str) -> bool {
    unsafe {
        for (i, wb) in want.bytes().enumerate() {
            let b = p.add(i).read();
            if b == 0 || b.to_ascii_lowercase() != wb.to_ascii_lowercase() {
                return false;
            }
        }
        p.add(want.len()).read() == 0
    }
}

unsafe fn hook_iat(module: HMODULE, dll: &str, func: &str, replacement: *const c_void) -> Option<*const c_void> {
    unsafe {
        let base = module.0 as *const u8;
        let e_lfanew = read_u32(base, 0x3C) as usize; // IMAGE_DOS_HEADER.e_lfanew
        // IMAGE_OPTIONAL_HEADER64 begins at nt + 4 (signature) + 20 (file header); DataDirectory at
        // optional+112; the import directory is entry 1 (each entry is 8 bytes).
        let import_dir_rva = read_u32(base, e_lfanew + 24 + 112 + 8) as usize;
        if import_dir_rva == 0 {
            return None;
        }

        let mut desc = base.add(import_dir_rva);
        loop {
            let original_first_thunk = read_u32(desc, 0) as usize;
            let name_rva = read_u32(desc, 12) as usize;
            let first_thunk = read_u32(desc, 16) as usize;
            if name_rva == 0 && first_thunk == 0 {
                break; // null terminator
            }
            if name_rva != 0 && cstr_eq_ignore_case(base.add(name_rva), dll) {
                let int_rva = if original_first_thunk != 0 { original_first_thunk } else { first_thunk };
                let mut i = 0usize;
                loop {
                    let int_entry = read_u64(base, int_rva + i * 8);
                    if int_entry == 0 {
                        break;
                    }
                    // Skip ordinal imports (high bit set); named imports point at IMAGE_IMPORT_BY_NAME.
                    if int_entry & (1 << 63) == 0 {
                        let name_ptr = base.add((int_entry as usize) + 2); // past the 2-byte Hint
                        if cstr_eq_ignore_case(name_ptr, func) {
                            let slot = base.add(first_thunk + i * 8) as *mut usize;
                            let mut old = PAGE_PROTECTION_FLAGS(0);
                            if VirtualProtect(slot as *const c_void, 8, PAGE_READWRITE, &mut old).is_ok() {
                                let original = slot.read();
                                slot.write(replacement as usize);
                                let _ = VirtualProtect(slot as *const c_void, 8, old, &mut old);
                                return Some(original as *const c_void);
                            }
                        }
                    }
                    i += 1;
                }
            }
            desc = desc.add(20); // sizeof(IMAGE_IMPORT_DESCRIPTOR)
        }
        None
    }
}

fn install_raw_input_hooks() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        if let Some(orig) =
            hook_iat(module, "user32.dll", "GetRawInputData", hooked_get_raw_input_data as *const c_void)
        {
            REAL_GET_RAW_INPUT_DATA.store(orig as usize, Ordering::Release);
        }
        if let Some(orig) = hook_iat(
            module,
            "user32.dll",
            "RegisterRawInputDevices",
            hooked_register_raw_input_devices as *const c_void,
        ) {
            REAL_REGISTER_RAW_INPUT_DEVICES.store(orig as usize, Ordering::Release);
        }
    }
}

/// Subclass `hwnd` and install the raw-input IAT hooks so input flows through the gate.
///
/// # Safety
/// `hwnd` must be a valid top-level window owned by this process.
pub unsafe fn install(hwnd: HWND) {
    unsafe {
        // Publish the original WndProc *before* ours becomes reachable. `install` runs on the attach
        // thread, not the window's UI thread, so a message could otherwise hit our WndProc while
        // `ORIG_WNDPROC` is still 0.
        let prev = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        ORIG_WNDPROC.store(prev, Ordering::Release);
        install_raw_input_hooks();
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as isize);
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() && IsWindowVisible(hwnd).as_bool() {
            *(lparam.0 as *mut HWND) = hwnd;
            return BOOL(0); // found it; stop enumerating
        }
        BOOL(1)
    }
}

fn find_main_window() -> Option<HWND> {
    let mut found = HWND(std::ptr::null_mut());
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut found as *mut HWND as isize));
    }
    (!found.0.is_null()).then_some(found)
}

fn host_port() -> u16 {
    std::env::var("UW_SPIKE_HOST_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(48610)
}

/// Own the socket on a background thread so the game's message pump never blocks on a slow or
/// stalled host. Connects (with retry), greets, then drains the queue to the wire.
fn run_sender(rx: std::sync::mpsc::Receiver<Msg>) {
    let port = host_port();
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    };
    let hello = Msg::Hello { protocol: PROTOCOL, pid: std::process::id() };
    if write_msg(&mut stream, &hello).is_err() {
        return;
    }
    while let Ok(msg) = rx.recv() {
        if write_msg(&mut stream, &msg).is_err() {
            break; // host went away; the hook keeps running as pure passthrough
        }
    }
}

/// Wire up the non-blocking host channel, find the game window, and install the subclass + hooks.
fn attach() {
    let (tx, rx) = sync_channel::<Msg>(1024);
    std::thread::spawn(move || run_sender(rx));
    let _ = STATE.set(Mutex::new(HookState { gate: InputGate::new(), host: ChannelHost { tx } }));

    let hwnd = loop {
        if let Some(h) = find_main_window() {
            break h;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    unsafe { install(hwnd) };
}

const DLL_PROCESS_ATTACH: u32 = 1;

/// Entry point when the DLL is loaded into the game process under Proton.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_hinst: HINSTANCE, reason: u32, _reserved: *mut core::ffi::c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        std::thread::spawn(attach);
    }
    BOOL(1)
}
