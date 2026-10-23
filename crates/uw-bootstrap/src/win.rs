//! The Windows side of the bootstrap: the exported hook procedure, the self-pin, the worker
//! thread (pipe connect + overlay load), and a do-nothing `DllMain`. Built only for
//! `x86_64-pc-windows-gnu`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, HINSTANCE, HMODULE,
    INVALID_HANDLE_VALUE, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GetProcAddress, LoadLibraryW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows_sys::Win32::System::Pipes::{SetNamedPipeHandleState, PIPE_READMODE_MESSAGE};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId, Sleep};
use windows_sys::Win32::UI::WindowsAndMessaging::CallNextHookEx;

use uw_pipe_proto::names::{marker_mutex_name, pipe_name};
use uw_pipe_proto::pipe::{decode, encode, Message, ProtoError, StatusCode, HEADER_LEN, MAX_PAYLOAD, PROTOCOL_VERSION};

use crate::state::{Decision, OverlayState};

static STARTED: AtomicBool = AtomicBool::new(false);

const BOOTSTRAP_VERSION: u16 = 1;
const START_PARAMS_VERSION: u32 = 1;
const RETRY_MS: u32 = 1000;

/// Does nothing on purpose: this DLL is also mapped into the helper, so `DllMain` must not touch
/// the loader lock or start work (design: loader-lock rule). Returns TRUE.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_inst: HINSTANCE, _reason: u32, _reserved: *mut core::ffi::c_void) -> i32 {
    1
}

/// The thread-hook procedure. Wine calls it inside the game once the UI thread pumps a message.
/// On the first call only, pin and start one worker thread; always `CallNextHookEx`.
///
/// # Safety
/// Matches the Win32 `HOOKPROC` ABI; called by the OS.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn uw_bootstrap_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // No Rust panic may cross this FFI boundary (since Rust 1.81 it aborts, killing the game).
    // `code < 0` must go straight to `CallNextHookEx` (HOOKPROC contract), so only `code >= 0` may
    // start the worker.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // The worker starts only once the pin holds: unpinned, the helper's unhook could unload
        // code the worker is still running.
        if code >= 0 && !STARTED.swap(true, Ordering::AcqRel) && pin_self() {
            let _ = std::thread::Builder::new().name("uw-bootstrap".into()).spawn(worker);
        }
    }));
    // SAFETY: a null hook handle forwards to the next hook (design: always).
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Pins this module so unhooking in the helper cannot unload it (design: `FROM_ADDRESS | PIN`).
/// Returns whether the pin took.
fn pin_self() -> bool {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS passes an address in this module as the "name"; PIN keeps it loaded.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            pin_self as fn() -> bool as usize as *const u16,
            &mut module,
        ) != 0
    }
}

/// The overlay entry ABI (design "Overlay entry point"). Extra fields may be appended later; the
/// test overlay ignores the pointer entirely. `uw_overlay_start` MUST catch its own panics: a
/// panic unwinding out of an `extern "system"` callee aborts at its own boundary (Rust >= 1.81),
/// which the caller's `catch_unwind` below cannot intercept.
#[repr(C)]
struct StartParams {
    size: u32,
    version: u32,
    pid: u32,
}

type OverlayStart = unsafe extern "system" fn(*const StartParams) -> u32;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The worker thread (design steps 5–8). Started once, on the first hook call. Creates the marker
/// mutex, then connects and serves the pipe; on a lost connection it reconnects and starts over
/// with a fresh `hello`.
fn worker() {
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    // Keep the marker mutex handle for the life of the process: a new helper that sees it skips
    // the hook and waits for our reconnect `hello` (design step 3).
    let _marker = create_marker(pid);
    let mut state = OverlayState::new();
    loop {
        // Catch per connection, not around the whole loop: a panic in `serve_once` ends only that
        // connection, so the worker keeps the marker alive and reconnects (otherwise every later
        // helper would see the marker, skip the hook and time out until the game restarts).
        if catch_unwind(AssertUnwindSafe(|| serve_once(pid, &mut state))).is_err() {
            eprintln!("[uw-bootstrap] serve panicked; reconnecting");
        }
        // SAFETY: a plain sleep.
        unsafe { Sleep(RETRY_MS) };
    }
}

fn create_marker(pid: u32) -> HANDLE {
    let name = wide(&marker_mutex_name(pid));
    // SAFETY: NUL-terminated name; null security attributes; not the initial owner.
    unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) }
}

/// Connect to the pipe (retry while missing/busy), send `hello`, then serve requests until the
/// pipe breaks. The handle closes when `pipe` drops, on return or on unwind.
fn serve_once(pid: u32, state: &mut OverlayState) {
    let pipe = loop {
        match connect(pid) {
            Some(p) => break p,
            None => {
                // SAFETY: a plain sleep before retrying (design: "retry once a second").
                unsafe { Sleep(RETRY_MS) };
            }
        }
    };
    let hello = Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: BOOTSTRAP_VERSION,
        bitness: 64,
        overlay_loaded: state.loaded_path().is_some(),
        overlay_path: state.loaded_path().unwrap_or_default().to_string(),
    };
    if write_message(pipe.0, &hello).is_err() {
        return;
    }
    let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
    // `None` means the pipe broke: fall out and reconnect.
    while let Some(bytes) = read_message(pipe.0, &mut buf) {
        if !handle(pipe.0, &buf[..bytes], pid, state) {
            break; // a bad frame closes the connection (design: bootstrap reconnects)
        }
    }
}

/// Owns the pipe client handle and closes it on drop. A panic in `serve_once` then still frees
/// the helper's single pipe instance; a leaked handle would keep it connected, so every reconnect
/// would get `ERROR_PIPE_BUSY` until the helper exits.
struct Pipe(HANDLE);

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: closes the handle this guard owns, exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Open the pipe client; `None` while the server pipe is missing or busy (both mean "retry").
fn connect(pid: u32) -> Option<Pipe> {
    let name = wide(&pipe_name(pid));
    // SAFETY: NUL-terminated name; opening an existing pipe for duplex message I/O.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_NONE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // The server pipe is missing (ERROR_FILE_NOT_FOUND) or its instance is taken
        // (ERROR_PIPE_BUSY); both mean "retry later" (design "Platform facts").
        return None;
    }
    let mode = PIPE_READMODE_MESSAGE;
    // SAFETY: switch our end to message read mode so one ReadFile yields one message.
    unsafe {
        SetNamedPipeHandleState(h, &mode, std::ptr::null(), std::ptr::null());
    }
    Some(Pipe(h))
}

/// Handle one decoded request; returns `false` to close the connection.
fn handle(pipe: HANDLE, bytes: &[u8], pid: u32, state: &mut OverlayState) -> bool {
    match decode(bytes) {
        Ok(Message::LoadOverlay { request_id, path }) => {
            let status = load_overlay(request_id, &path, pid, state);
            write_message(pipe, &status).is_ok()
        }
        // The helper never sends these to us; ignore and keep serving.
        Ok(_) => true,
        // Unknown type: reply `unsupported` and keep serving (design "Messages").
        Err(ProtoError::UnknownType(_)) => {
            let s = Message::Status {
                request_id: 0,
                code: StatusCode::Unsupported,
                detail: 0,
                message: "unknown message type".into(),
            };
            write_message(pipe, &s).is_ok()
        }
        // A bad magic or oversized/garbled frame closes the connection.
        Err(_) => false,
    }
}

fn load_overlay(request_id: u32, path: &str, pid: u32, state: &mut OverlayState) -> Message {
    let status = |code, detail, message: &str| Message::Status {
        request_id,
        code,
        detail,
        message: message.to_string(),
    };
    match state.decide(path) {
        Decision::AlreadyLoaded => return status(StatusCode::AlreadyLoaded, 0, "overlay already loaded"),
        Decision::AlreadyLoadedOther => {
            return status(StatusCode::AlreadyLoadedOther, 0, "a different overlay is already loaded");
        }
        Decision::Load => {}
    }
    let wpath = wide(path);
    // SAFETY: NUL-terminated path; loads on the worker thread (never DllMain) per the loader-lock
    // rule.
    let module: HMODULE = unsafe { LoadLibraryW(wpath.as_ptr()) };
    if module.is_null() {
        // SAFETY: no arguments.
        let e = unsafe { GetLastError() };
        return status(StatusCode::LoadFailed, e, "LoadLibraryW failed");
    }
    // SAFETY: the export matches the overlay ABI.
    let proc = unsafe { GetProcAddress(module, c"uw_overlay_start".as_ptr() as *const u8) };
    let Some(proc) = proc else {
        return status(StatusCode::NoStartExport, 0, "no uw_overlay_start export");
    };
    // SAFETY: transmute to the overlay ABI and call it with StartParams.
    let start: OverlayStart = unsafe { std::mem::transmute::<_, OverlayStart>(proc) };
    let params = StartParams { size: std::mem::size_of::<StartParams>() as u32, version: START_PARAMS_VERSION, pid };
    // This catch guards our own glue around the call; it can NOT catch a panic inside the overlay
    // (that aborts at the overlay's `extern "system"` boundary), so `uw_overlay_start` must catch
    // its own panics (design "Failures"; noted on `StartParams`).
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the overlay owns `*params` only for the call.
        unsafe { start(&params) }
    }));
    match r {
        Ok(0) => {
            state.mark_loaded(path.to_string());
            status(StatusCode::Ok, 0, "overlay started")
        }
        Ok(code) => status(StatusCode::StartFailed, code, "uw_overlay_start returned nonzero"),
        Err(_) => status(StatusCode::StartFailed, 0, "uw_overlay_start panicked"),
    }
}

fn write_message(pipe: HANDLE, m: &Message) -> Result<(), ()> {
    let bytes = encode(m);
    let mut written = 0u32;
    // SAFETY: message-mode write of one framed message.
    let ok = unsafe { WriteFile(pipe, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut()) };
    if ok != 0 && written as usize == bytes.len() { Ok(()) } else { Err(()) }
}

/// One message in message mode, or `None` when the pipe is broken/closed.
fn read_message(pipe: HANDLE, buf: &mut [u8]) -> Option<usize> {
    let mut read = 0u32;
    // SAFETY: reads one message into `buf` (sized HEADER_LEN + MAX_PAYLOAD, the maximum).
    let ok = unsafe { ReadFile(pipe, buf.as_mut_ptr(), buf.len() as u32, &mut read, std::ptr::null_mut()) };
    if ok != 0 && read > 0 { Some(read as usize) } else { None }
}
