//! The real `Win32` for `decide`: Toolhelp and window lookups, the helper and marker mutexes, the
//! thread hook and its wake-ups, and the bootstrap pipe (a `PipeServer`).

use std::panic::{catch_unwind, AssertUnwindSafe};

use uw_helper::decide::{HelloWait, HookKind, Win32};
use uw_pipe_proto::names::{helper_mutex_name, marker_mutex_name, pipe_name};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM,
    LRESULT, RECT, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenMutexW, OpenProcess, Sleep, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, PostThreadMessageW,
    SendNotifyMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GW_OWNER, HHOOK, HOOKPROC, WH_CALLWNDPROC,
    WH_GETMESSAGE, WM_NULL,
};

use crate::pipe_server::PipeServer;
use crate::{wait_ms, wide};

fn basename_eq(name: &[u16], target: &str) -> bool {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let s = String::from_utf16_lossy(&name[..end]);
    let base = s.rsplit(['\\', '/']).next().unwrap_or(&s);
    base.eq_ignore_ascii_case(target)
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

pub(crate) struct RealWin32 {
    exe: String,
    bootstrap: String,
    /// The game's Windows pid, once `find_game` found it.
    pub(crate) pid: u32,
    hwnd: HWND,
    hook: HHOOK,
    module: HMODULE,
    helper_mutex: HANDLE,
    /// The bootstrap pipe, from `create_pipe` on.
    pub(crate) pipe: Option<PipeServer>,
}

impl RealWin32 {
    pub(crate) fn new(exe: String, bootstrap: String) -> RealWin32 {
        RealWin32 {
            exe,
            bootstrap,
            pid: 0,
            hwnd: std::ptr::null_mut(),
            hook: std::ptr::null_mut(),
            module: std::ptr::null_mut(),
            helper_mutex: std::ptr::null_mut(),
            pipe: None,
        }
    }

    fn load_bootstrap(&mut self) -> Option<HOOKPROC> {
        if self.module.is_null() {
            let w = wide(&self.bootstrap);
            // SAFETY: NUL-terminated path.
            self.module = unsafe { LoadLibraryW(w.as_ptr()) };
            if self.module.is_null() {
                return None;
            }
        }
        // SAFETY: the export matches the HOOKPROC ABI.
        let p = unsafe { GetProcAddress(self.module, c"uw_bootstrap_hook".as_ptr() as *const u8) }?;
        // SAFETY: transmute the resolved export (`FARPROC`'s inner fn type, named so the source
        // type is pinned) to the hook procedure type it really has.
        Some(Some(unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
            >(p)
        }))
    }
}

impl Drop for RealWin32 {
    fn drop(&mut self) {
        self.unhook();
        if !self.helper_mutex.is_null() {
            // SAFETY: closes the helper mutex handle this helper created and still owns.
            unsafe { CloseHandle(self.helper_mutex) };
        }
        // `pipe` drops after this: it cancels any pending accept and closes its handles.
    }
}

impl Win32 for RealWin32 {
    fn now_ms(&self) -> u64 {
        // SAFETY: a monotonic tick count is fine for the window/hello timeouts.
        unsafe { GetTickCount64() }
    }
    fn sleep_ms(&mut self, ms: u64) {
        // SAFETY: a plain sleep.
        unsafe { Sleep(wait_ms(ms)) };
    }
    fn find_game(&mut self) -> Option<u32> {
        // SAFETY: a process snapshot walked with a correctly sized PROCESSENTRY32W, closed on
        // every path (`run` polls, so a leak would grow every 500 ms).
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
                if e.th32ProcessID != GetCurrentProcessId() && basename_eq(&e.szExeFile, &self.exe) {
                    found = Some(e.th32ProcessID);
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            if let Some(pid) = found {
                self.pid = pid;
            }
            found
        }
    }
    fn find_ui_thread(&mut self, pid: u32) -> Option<u32> {
        let mut f = Found { pid, best_area: 0, thread: 0, hwnd: std::ptr::null_mut() };
        // SAFETY: the callback only writes through `&mut f`.
        unsafe { EnumWindows(Some(enum_cb), &mut f as *mut Found as LPARAM) };
        if f.thread != 0 {
            self.hwnd = f.hwnd;
            Some(f.thread)
        } else {
            None
        }
    }
    fn game_alive(&mut self, pid: u32) -> bool {
        // SAFETY: opens the game for SYNCHRONIZE, polls it without waiting, closes the handle.
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            CloseHandle(h);
            alive
        }
    }
    fn marker_exists(&mut self, pid: u32) -> bool {
        let name = wide(&marker_mutex_name(pid));
        // SAFETY: NUL-terminated name.
        let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
        if h.is_null() {
            false
        } else {
            // SAFETY: close the probe handle; the owner keeps the mutex alive.
            unsafe { CloseHandle(h) };
            true
        }
    }
    fn take_helper_lock(&mut self, pid: u32) -> bool {
        let name = wide(&helper_mutex_name(pid));
        // SAFETY: NUL-terminated name; keep the handle for the process lifetime.
        self.helper_mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: no arguments; read right after CreateMutexW.
        unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
    }
    fn create_pipe(&mut self, pid: u32) -> Result<(), String> {
        self.pipe = Some(PipeServer::create(&pipe_name(pid))?);
        Ok(())
    }
    fn set_hook(&mut self, kind: HookKind, thread: u32) -> Result<(), u32> {
        let Some(hookproc) = self.load_bootstrap() else {
            // SAFETY: no arguments.
            return Err(unsafe { GetLastError() });
        };
        let id = match kind {
            HookKind::GetMessage => WH_GETMESSAGE,
            HookKind::CallWndProc => WH_CALLWNDPROC,
        };
        // SAFETY: thread-scoped hook with a DLL-resident procedure and that DLL's module.
        let hook = unsafe { SetWindowsHookExW(id, hookproc, self.module, thread) };
        if hook.is_null() {
            // SAFETY: no arguments.
            Err(unsafe { GetLastError() })
        } else {
            self.hook = hook;
            let name = match kind {
                HookKind::GetMessage => "WH_GETMESSAGE",
                HookKind::CallWndProc => "WH_CALLWNDPROC",
            };
            eprintln!("[uw-helper64] hooked {name} thread={thread}");
            Ok(())
        }
    }
    /// Wake the UI thread so its next message pump runs the hook: posted `WM_NULL`s
    /// (`WH_GETMESSAGE`) and one sent `WM_NULL` to the main window (`WH_CALLWNDPROC` only
    /// fires on sent messages). `decide` calls this after each hook and every `REWAKE_MS`.
    fn post_wakeups(&mut self, thread: u32) {
        // SAFETY: posts WM_NULL to the UI thread and send-notifies its main window.
        unsafe {
            for _ in 0..5 {
                PostThreadMessageW(thread, WM_NULL, 0, 0);
            }
            if !self.hwnd.is_null() {
                SendNotifyMessageW(self.hwnd, WM_NULL, 0, 0);
            }
        }
    }
    fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait {
        match self.pipe.as_mut() {
            Some(p) => p.accept_hello(timeout_ms),
            None => HelloWait::PipeError("no pipe".into()),
        }
    }
    fn unhook(&mut self) {
        if !self.hook.is_null() {
            // SAFETY: the bootstrap's self-pin keeps it loaded after unhooking.
            unsafe { UnhookWindowsHookEx(self.hook) };
            self.hook = std::ptr::null_mut();
        }
    }
    fn log(&self, msg: &str) {
        eprintln!("[uw-helper64] {msg}");
    }
}
