//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Task 1 proof only: find the game by exe name, find its UI thread, `LoadLibraryW` the bootstrap
//! into this process, `SetWindowsHookExW(WH_GETMESSAGE | WH_CALLWNDPROC)` on the UI thread, post a
//! few `WM_NULL` messages, and wait up to 10 s for the bootstrap's proof file to appear. Prints
//! `proof <path>` on success, `no_window` / `hook_failed <err>` / `no_proof` otherwise.
//! `--fallback` forces `WH_CALLWNDPROC`. Task 6 replaces this with the full pipe-serving helper.

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-helper64 runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::Storage::FileSystem::GetTempPathW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
        PostThreadMessageW, SendNotifyMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GW_OWNER,
        HHOOK, HOOKPROC, WH_CALLWNDPROC, WH_GETMESSAGE, WINDOWS_HOOK_ID, WM_NULL,
    };
    use windows_sys::Win32::Foundation::{HMODULE, LRESULT, WPARAM};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn basename_eq(name: &[u16], target: &str) -> bool {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let s = String::from_utf16_lossy(&name[..end]);
        let base = s.rsplit(['\\', '/']).next().unwrap_or(&s);
        base.eq_ignore_ascii_case(target)
    }

    fn find_game(exe: &str) -> Option<u32> {
        // SAFETY: a process snapshot walked with correctly sized PROCESSENTRY32W, closed on
        // every path (the caller polls, so a leak would grow every 500 ms).
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut found = None;
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                if e.th32ProcessID != GetCurrentProcessId() && basename_eq(&e.szExeFile, exe) {
                    found = Some(e.th32ProcessID);
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            found
        }
    }

    struct Found {
        pid: u32,
        best_area: i64,
        thread: u32,
        hwnd: HWND,
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        // No Rust panic may cross this FFI boundary: catch and keep enumerating.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `lparam` points at the caller's `Found`.
            let f = unsafe { &mut *(lparam as *mut Found) };
            let mut pid = 0u32;
            // SAFETY: `hwnd` from EnumWindows; writes one u32.
            let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            // SAFETY: querying visibility/owner/rect of an enumerated top-level window.
            unsafe {
                if pid == f.pid && IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() {
                    let mut r: RECT = std::mem::zeroed();
                    if GetWindowRect(hwnd, &mut r) != 0 {
                        let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                        if area > f.best_area {
                            f.best_area = area;
                            f.thread = thread;
                            f.hwnd = hwnd;
                        }
                    }
                }
            }
        }));
        1 // keep enumerating
    }

    /// The thread and window of the game's largest visible top-level window without an owner.
    fn ui_thread(pid: u32) -> Option<(u32, HWND)> {
        let mut f = Found { pid, best_area: 0, thread: 0, hwnd: std::ptr::null_mut() };
        // SAFETY: the callback only writes through `&mut f`.
        unsafe {
            EnumWindows(Some(enum_cb), &mut f as *mut Found as LPARAM);
        }
        (f.thread != 0).then_some((f.thread, f.hwnd))
    }

    fn proof_path(pid: u32) -> std::path::PathBuf {
        // 32768 like the bootstrap: `GetTempPathW` never needs more, so it never returns the
        // "required size" (with nothing written) that a MAX_PATH buffer could get.
        let mut buf = [0u16; 32768];
        // SAFETY: `buf` writable for its length.
        let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let dir = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        std::path::PathBuf::from(dir).join(format!("uw-bootstrap-proof-{pid}.txt"))
    }

    pub fn main() -> i32 {
        let mut args = std::env::args().skip(1);
        let exe = args.next().unwrap_or_default();
        let mut bootstrap = String::new();
        let mut fallback = false;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--bootstrap" => bootstrap = args.next().unwrap_or_default(),
                "--fallback" => fallback = true,
                other => {
                    eprintln!("[uw-helper64] unknown argument {other}");
                    return 2;
                }
            }
        }
        if exe.is_empty() || bootstrap.is_empty() {
            eprintln!("usage: uw-helper64 <game-exe> --bootstrap <path> [--fallback]");
            return 2;
        }

        let start = Instant::now();
        let (pid, thread, hwnd) = loop {
            if let Some(pid) = find_game(&exe)
                && let Some((t, h)) = ui_thread(pid)
            {
                break (pid, t, h);
            }
            if start.elapsed() > Duration::from_secs(120) {
                println!("no_window");
                return 3;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        eprintln!("[uw-helper64] {exe} pid {pid}, ui thread {thread}");

        // Load the bootstrap into THIS process to get its module handle and the hook proc address
        // (design §"Platform facts": SetWindowsHookExW needs a DLL-resident procedure).
        let wpath = wide(&bootstrap);
        // SAFETY: NUL-terminated path.
        let module: HMODULE = unsafe { LoadLibraryW(wpath.as_ptr()) };
        if module.is_null() {
            println!("hook_failed {}", last_error());
            return 4;
        }
        // SAFETY: the export matches the HOOKPROC ABI.
        let proc_addr = unsafe { GetProcAddress(module, c"uw_bootstrap_hook".as_ptr() as *const u8) };
        let Some(proc_addr) = proc_addr else {
            println!("hook_failed no_export");
            return 4;
        };
        // SAFETY: transmute the resolved export (`FARPROC`'s inner fn type, named so the source
        // type is pinned) to the hook procedure type it really has.
        let hookproc: HOOKPROC = Some(unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
            >(proc_addr)
        });

        let kind = if fallback { WH_CALLWNDPROC } else { WH_GETMESSAGE };
        // SAFETY: a thread-scoped hook with a DLL-resident procedure and that DLL's module handle.
        let hook: HHOOK = unsafe { SetWindowsHookExW(kind, hookproc, module, thread) };
        if hook.is_null() {
            // Try the other hook once.
            let other = if fallback { WH_GETMESSAGE } else { WH_CALLWNDPROC };
            // SAFETY: as above.
            let hook2 = unsafe { SetWindowsHookExW(other, hookproc, module, thread) };
            if hook2.is_null() {
                println!("hook_failed {}", last_error());
                return 4;
            }
            eprintln!("[uw-helper64] hooked {} thread={thread}", hook_name(other));
            return wait_proof(pid, thread, hwnd, hook2);
        }
        eprintln!("[uw-helper64] hooked {} thread={thread}", hook_name(kind));
        wait_proof(pid, thread, hwnd, hook)
    }

    fn hook_name(kind: WINDOWS_HOOK_ID) -> &'static str {
        if kind == WH_CALLWNDPROC { "WH_CALLWNDPROC" } else { "WH_GETMESSAGE" }
    }

    /// Wake the UI thread so the next message pump runs the hook: a posted `WM_NULL`
    /// (`WH_GETMESSAGE`) and a sent `WM_NULL` to the main window (`WH_CALLWNDPROC`, which only
    /// fires on sent messages).
    fn wake(thread: u32, hwnd: HWND) {
        // SAFETY: posts/sends WM_NULL to the UI thread and its window.
        unsafe {
            PostThreadMessageW(thread, WM_NULL, 0, 0);
            if !hwnd.is_null() {
                SendNotifyMessageW(hwnd, WM_NULL, 0, 0);
            }
        }
    }

    fn wait_proof(pid: u32, thread: u32, hwnd: HWND, hook: HHOOK) -> i32 {
        for _ in 0..5 {
            wake(thread, hwnd);
        }
        let path = proof_path(pid);
        let start = Instant::now();
        let found = loop {
            if path.exists() {
                break true;
            }
            if start.elapsed() > Duration::from_secs(10) {
                break false;
            }
            wake(thread, hwnd); // re-wake every 200 ms, as the design's "despite wake-ups" requires
            std::thread::sleep(Duration::from_millis(200));
        };
        // SAFETY: the pin keeps the bootstrap loaded after unhooking.
        unsafe { UnhookWindowsHookEx(hook) };
        if found {
            println!("proof {}", path.display());
            0
        } else {
            println!("no_proof");
            5
        }
    }

    fn last_error() -> u32 {
        // SAFETY: no arguments.
        unsafe { windows_sys::Win32::Foundation::GetLastError() }
    }
}
