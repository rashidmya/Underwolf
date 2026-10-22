//! The Windows side of the bootstrap: the exported hook procedure, the self-pin, the worker
//! thread, and a do-nothing `DllMain`. Built only for `x86_64-pc-windows-gnu`.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HINSTANCE, HMODULE, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::CallNextHookEx;
use windows_sys::Win32::Foundation::{GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};

static STARTED: AtomicBool = AtomicBool::new(false);

/// Does nothing on purpose. The bootstrap is also mapped into the helper, so `DllMain` must not
/// touch the loader lock or start work (design: loader-lock rule). Returns TRUE.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_inst: HINSTANCE, reason: u32, _reserved: *mut core::ffi::c_void) -> i32 {
    let _ = reason;
    let _ = DLL_PROCESS_ATTACH;
    1
}

/// The thread-hook procedure. Wine calls it inside the game once the UI thread pumps a message.
///
/// # Safety
/// Matches the Win32 `HOOKPROC` ABI; called by the OS.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn uw_bootstrap_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // No Rust panic may cross this FFI boundary (since Rust 1.81 it aborts, which would kill the
    // game). Catch it; the call always ends in `CallNextHookEx`.
    let _ = std::panic::catch_unwind(|| {
        if !STARTED.swap(true, Ordering::AcqRel) {
            pin_self();
            // One worker thread; nothing else runs on the game's thread.
            let _ = std::thread::Builder::new()
                .name("uw-bootstrap".into())
                .spawn(worker);
        }
    });
    // SAFETY: `CallNextHookEx` with a null hook handle forwards to the next hook (design: always).
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Pins this module so unhooking in the helper cannot unload it (design: `FROM_ADDRESS | PIN`).
fn pin_self() {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS passes an address in this module as the "name"; PIN keeps it loaded.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            pin_self as fn() as usize as *const u16,
            &mut module,
        );
    }
}

fn worker() {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let mut path = temp_dir_wide();
        for c in format!("uw-bootstrap-proof-{pid}.txt\0").encode_utf16() {
            path.push(c);
        }
        write_file(&path, format!("bootstrap in pid {pid}\n").as_bytes());
    });
}

/// `%TEMP%` as a NUL-less wide buffer ending in a path separator (via `GetTempPathW`).
fn temp_dir_wide() -> Vec<u16> {
    use windows_sys::Win32::Storage::FileSystem::GetTempPathW;
    let mut buf = [0u16; 32768];
    // SAFETY: `buf` is writable for its length; returns the count written (no NUL counted).
    let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    buf[..n.min(buf.len())].to_vec()
}

fn write_file(path_wide_nul: &[u16], bytes: &[u8]) {
    // SAFETY: `path_wide_nul` is NUL-terminated; a new file is created and the bytes written.
    unsafe {
        let h: HANDLE = CreateFileW(
            path_wide_nul.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            std::ptr::null(),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        let mut written = 0u32;
        let _ = WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut());
        windows_sys::Win32::Foundation::CloseHandle(h);
    }
}
