//! A stand-in game for the Wine end-to-end tests. `uw-test-game` opens one visible top-level
//! window (no owner) and runs a standard `GetMessage`/`TranslateMessage`/`DispatchMessage` loop,
//! so a thread-targeted `WH_GETMESSAGE`/`WH_CALLWNDPROC` hook has a UI thread to run on.
//! `uw-test-game --no-window` sleeps with no window (for the `no_window` scenario).
//! `uw-test-game --bad-state-block` first creates `Local\underwolf-<pid>-state` with a layout v2
//! header and holds it, as a game would that kept a block from another client version (for the
//! `version_mismatch` scenario).

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-test-game is a Windows program; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--bad-state-block") {
        win::hold_bad_state_block();
    }
    if std::env::args().any(|a| a == "--no-window") {
        // No window: the helper must give up with `no_window`.
        std::thread::sleep(std::time::Duration::from_secs(300));
        return;
    }
    win::run();
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
        RegisterClassW, ShowWindow, TranslateMessage, CW_USEDEFAULT, MSG, SW_SHOW, WM_DESTROY,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows_sys::core::w;

    /// Create the state block for our own pid with a header the helper must refuse: right magic
    /// and size, layout version 2. The handle and view are kept for the life of the process.
    pub fn hold_bad_state_block() {
        use uw_pipe_proto::names::state_name;
        use uw_pipe_proto::state::{init_header, BLOCK_SIZE};
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_WRITE, PAGE_READWRITE};
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;

        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let name: Vec<u16> = state_name(pid).encode_utf16().chain(Some(0)).collect();
        // SAFETY: a named paging-file mapping and a write view of it, both deliberately kept open
        // (never closed or unmapped) until the process exits.
        unsafe {
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, BLOCK_SIZE as u32, name.as_ptr());
            assert!(!h.is_null(), "CreateFileMappingW failed");
            let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 0);
            assert!(!view.Value.is_null(), "MapViewOfFile failed");
            let b = std::slice::from_raw_parts_mut(view.Value.cast::<u8>(), BLOCK_SIZE);
            init_header(b);
            b[4..6].copy_from_slice(&2u16.to_le_bytes()); // layout version 2
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            // SAFETY: standard teardown.
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        // SAFETY: default handling for every other message.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub fn run() {
        // SAFETY: Win32 window creation with a static class name and our own wndproc.
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: w!("UwTestGameWindow"),
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                0,
                w!("UwTestGameWindow"),
                w!("uw-test-game"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                640,
                480,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            assert!(!hwnd.is_null(), "CreateWindowExW failed");
            ShowWindow(hwnd, SW_SHOW);
            let mut msg: MSG = std::mem::zeroed();
            // GetMessageW returns 0 on WM_QUIT, -1 on error.
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
