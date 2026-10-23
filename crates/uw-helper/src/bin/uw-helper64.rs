//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Finds the game's UI thread and sets a thread hook that
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`, …) on stdout, commands (`load_overlay`, …) on stdin, each reply echoing its
//! command's `request_id`; diagnostics on stderr.

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
    use std::io::{BufRead, Write};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use uw_helper::decide::{hello_wait_from_bytes, run, Config, HelloWait, HookKind, Outcome, Win32};
    use uw_pipe_proto::jsonl::{Command, Event};
    use uw_pipe_proto::names::{helper_mutex_name, marker_mutex_name, pipe_name};
    use uw_pipe_proto::pipe::{decode, encode, Message, HEADER_LEN, MAX_PATH_UNITS, MAX_PAYLOAD};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
        ERROR_PIPE_CONNECTED, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT,
        WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, GetCurrentProcessId, OpenMutexW, OpenProcess, ResetEvent, Sleep,
        WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
        PostThreadMessageW, SendNotifyMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GW_OWNER,
        HHOOK, HOOKPROC, WH_CALLWNDPROC, WH_GETMESSAGE, WM_NULL,
    };

    /// How long the relay waits for one pipe write, and for the bootstrap's `status` reply.
    const RELAY_TIMEOUT_MS: u64 = 15_000;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn emit(event: &Event) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", event.to_line());
        let _ = out.flush();
    }

    fn basename_eq(name: &[u16], target: &str) -> bool {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let s = String::from_utf16_lossy(&name[..end]);
        let base = s.rsplit(['\\', '/']).next().unwrap_or(&s);
        base.eq_ignore_ascii_case(target)
    }

    /// A wait timeout in ms for the Win32 wait functions, never `INFINITE`.
    fn wait_ms(ms: u64) -> u32 {
        u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1)
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

    /// Where the bootstrap's accept stands between `wait_hello` slices. `decide` calls
    /// `wait_hello` in slices of at most `REWAKE_MS`, so a slice that times out leaves the
    /// overlapped connect or read pending on `RealWin32::ov` for the next slice: it is never
    /// cancelled mid-wait and the bootstrap's connection is never dropped.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Accept {
        /// No I/O pending and no client: the next slice starts an overlapped `ConnectNamedPipe`.
        Listening,
        /// An overlapped `ConnectNamedPipe` is pending on `ov`.
        Connecting,
        /// A client is connected; an overlapped `ReadFile` of its first message is pending on
        /// `ov`, into `hello_buf`.
        Reading,
        /// The first message was read; the pipe now belongs to the relay.
        Connected,
    }

    struct RealWin32 {
        exe: String,
        bootstrap: String,
        pid: u32,
        hwnd: HWND,
        pipe: HANDLE,
        hook: HHOOK,
        module: HMODULE,
        helper_mutex: HANDLE,
        /// Manual-reset event for every overlapped operation (one at a time).
        event: HANDLE,
        /// The accept's `OVERLAPPED`, boxed so its address stays fixed while I/O is pending.
        ov: Box<OVERLAPPED>,
        hello_buf: Vec<u8>,
        accept: Accept,
    }

    impl RealWin32 {
        fn new(exe: String, bootstrap: String) -> RealWin32 {
            RealWin32 {
                exe,
                bootstrap,
                pid: 0,
                hwnd: std::ptr::null_mut(),
                pipe: INVALID_HANDLE_VALUE,
                hook: std::ptr::null_mut(),
                module: std::ptr::null_mut(),
                helper_mutex: std::ptr::null_mut(),
                event: std::ptr::null_mut(),
                // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
                ov: Box::new(unsafe { std::mem::zeroed() }),
                hello_buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
                accept: Accept::Listening,
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

        /// Cancel a pending overlapped op and wait for the cancel to finish, so its `OVERLAPPED`
        /// (and buffer) are safe to reuse or drop (plan-review I5).
        fn cancel(&self, ov: *const OVERLAPPED) {
            // SAFETY: cancels I/O on our pipe and blocks until the cancel completes.
            unsafe {
                CancelIoEx(self.pipe, ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, ov, &mut n, 1);
            }
        }

        /// Free the pipe instance after a client left before its first message (the bootstrap
        /// reconnects within a second), so the next slice can accept again.
        fn drop_client(&mut self) {
            // SAFETY: disconnects our pipe instance; no I/O is pending on it here.
            unsafe { DisconnectNamedPipe(self.pipe) };
            self.accept = Accept::Listening;
        }

        /// Start the overlapped connect on `ov`. Returns `Some` when this slice is over.
        fn start_connect(&mut self) -> Option<HelloWait> {
            // SAFETY: an overlapped connect whose `OVERLAPPED` lives in `self.ov` (boxed) until it
            // completes or is cancelled; the event is reset before it starts.
            let err = unsafe {
                *self.ov = std::mem::zeroed();
                self.ov.hEvent = self.event;
                ResetEvent(self.event);
                if ConnectNamedPipe(self.pipe, &mut *self.ov) != 0 { 0 } else { GetLastError() }
            };
            match err {
                ERROR_IO_PENDING => {
                    self.accept = Accept::Connecting;
                    None
                }
                // Connected already (the bootstrap won the race with this call): read its hello.
                0 | ERROR_PIPE_CONNECTED => self.start_read(),
                e => Some(HelloWait::PipeError(format!("ConnectNamedPipe: {e}"))),
            }
        }

        /// Start the overlapped read of the client's first message into `hello_buf`.
        fn start_read(&mut self) -> Option<HelloWait> {
            // SAFETY: an overlapped read into `hello_buf` (never resized) whose `OVERLAPPED` is
            // `self.ov`; both live in `self` until the read completes or is cancelled.
            let err = unsafe {
                *self.ov = std::mem::zeroed();
                self.ov.hEvent = self.event;
                ResetEvent(self.event);
                let ok = ReadFile(
                    self.pipe,
                    self.hello_buf.as_mut_ptr(),
                    self.hello_buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut *self.ov,
                );
                if ok != 0 { 0 } else { GetLastError() }
            };
            match err {
                0 => {
                    // Completed at once: collect the byte count from `ov`.
                    let mut n = 0u32;
                    // SAFETY: the read on `ov` has completed; no wait.
                    unsafe { GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) };
                    Some(self.take_hello(n))
                }
                ERROR_IO_PENDING => {
                    self.accept = Accept::Reading;
                    None
                }
                // The client left (broken pipe) or sent an oversized frame: drop it and accept
                // again; the bootstrap's connect loop retries.
                _ => {
                    self.drop_client();
                    Some(HelloWait::Timeout)
                }
            }
        }

        /// Classify the `n`-byte first message in `hello_buf`; the pipe then belongs to the relay.
        fn take_hello(&mut self, n: u32) -> HelloWait {
            if n == 0 {
                self.drop_client();
                return HelloWait::Timeout;
            }
            self.accept = Accept::Connected;
            // Portable and unit-tested; maps another header version to `version_mismatch`.
            hello_wait_from_bytes(&self.hello_buf[..n as usize])
        }

        /// Wait for the pending op on `ov` until `deadline`: `Ok(None)` while it is still
        /// pending (it stays pending), `Ok(Some(bytes))` once it completed, `Err` if it failed.
        fn wait_pending(&mut self, deadline: u64) -> Result<Option<u32>, u32> {
            let left = deadline.saturating_sub(self.now_ms());
            // SAFETY: waits on our event, then collects `ov`'s result without blocking.
            unsafe {
                if WaitForSingleObject(self.event, wait_ms(left)) != WAIT_OBJECT_0 {
                    return Ok(None);
                }
                let mut n = 0u32;
                if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                    return Ok(Some(n));
                }
                match GetLastError() {
                    ERROR_IO_INCOMPLETE => Ok(None),
                    e => Err(e),
                }
            }
        }

        /// One `wait_hello` slice: accept the bootstrap and read its first message, keeping any
        /// pending connect or read across slices (see `Accept`).
        fn accept_hello(&mut self, timeout_ms: u64) -> HelloWait {
            let deadline = self.now_ms().saturating_add(timeout_ms);
            loop {
                match self.accept {
                    Accept::Listening => {
                        if let Some(hw) = self.start_connect() {
                            return hw;
                        }
                    }
                    Accept::Connecting => match self.wait_pending(deadline) {
                        Ok(None) => return HelloWait::Timeout,
                        Ok(Some(_)) => {
                            if let Some(hw) = self.start_read() {
                                return hw;
                            }
                        }
                        // The client connected and left already: accept again.
                        Err(_) => {
                            self.drop_client();
                            return HelloWait::Timeout;
                        }
                    },
                    Accept::Reading => match self.wait_pending(deadline) {
                        Ok(None) => return HelloWait::Timeout,
                        Ok(Some(n)) => return self.take_hello(n),
                        // The client left before its hello, or sent an oversized frame.
                        Err(_) => {
                            self.drop_client();
                            return HelloWait::Timeout;
                        }
                    },
                    Accept::Connected => return HelloWait::PipeError("hello already taken".into()),
                }
            }
        }

        /// Read one message (message mode, overlapped) within `timeout_ms`.
        fn read_message(&self, timeout_ms: u64) -> Option<Vec<u8>> {
            let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
            // SAFETY: overlapped read into `buf` bounded by the event wait; a timed-out read is
            // cancelled (and the cancel awaited) before `ov` and `buf` go out of scope.
            unsafe {
                ResetEvent(self.event);
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = self.event;
                let ok = ReadFile(self.pipe, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), &mut ov);
                let mut n = 0u32;
                if ok == 0 {
                    if GetLastError() != ERROR_IO_PENDING {
                        return None;
                    }
                    if WaitForSingleObject(self.event, wait_ms(timeout_ms)) != WAIT_OBJECT_0 {
                        self.cancel(&ov);
                        return None;
                    }
                    if GetOverlappedResult(self.pipe, &ov, &mut n, 0) == 0 {
                        return None;
                    }
                } else {
                    GetOverlappedResult(self.pipe, &ov, &mut n, 0);
                }
                (n > 0).then(|| buf[..n as usize].to_vec())
            }
        }

        fn write_message(&self, m: &Message) -> bool {
            let bytes = encode(m);
            // SAFETY: overlapped write; wait for completion, cancelling (and awaiting the cancel)
            // on timeout before `ov` and `bytes` go out of scope.
            unsafe {
                ResetEvent(self.event);
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = self.event;
                let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
                if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                    return false;
                }
                if WaitForSingleObject(self.event, wait_ms(RELAY_TIMEOUT_MS)) != WAIT_OBJECT_0 {
                    self.cancel(&ov);
                    return false;
                }
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
            }
        }

        /// Relay host commands to the bootstrap and status replies back. A watcher thread reports
        /// `game_exited` the moment the game process ends, even while this blocks on stdin.
        fn relay(&mut self) {
            spawn_game_watcher(self.pid);
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match Command::parse(&line) {
                    Ok(Command::LoadOverlay { request_id, path }) => {
                        if path.encode_utf16().count() > MAX_PATH_UNITS {
                            emit(&Event::reply_error(request_id, "path_too_long"));
                            continue;
                        }
                        // The command's id travels to the bootstrap, which echoes it in `status`.
                        if !self.write_message(&Message::LoadOverlay { request_id, path }) {
                            // The bootstrap's end is gone: `bootstrap_disconnected`, not
                            // `game_exited` (design 2a, "Plan 1 items").
                            emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                            emit(&Event::BootstrapDisconnected);
                            return;
                        }
                        match self.read_message(RELAY_TIMEOUT_MS).and_then(|b| decode(&b).ok()) {
                            Some(Message::Status { request_id, code, detail, message }) => {
                                emit(&Event::Status { request_id, code: code.as_str().into(), detail, message });
                            }
                            _ => emit(&Event::reply_error(request_id, "status_timeout")),
                        }
                    }
                    // No overlay channel yet: commands that need the overlay fail (design: no
                    // queueing while disconnected).
                    Ok(cmd @ (Command::SetState { .. } | Command::Ping { .. })) => {
                        emit(&Event::reply_error(cmd.request_id(), "overlay_not_connected"));
                    }
                    Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
                }
            }
        }
    }

    impl Drop for RealWin32 {
        fn drop(&mut self) {
            // A pending accept still writes into `ov` and `hello_buf`: cancel it and wait for the
            // cancel before they are freed (plan-review I5).
            if matches!(self.accept, Accept::Connecting | Accept::Reading) {
                self.cancel(&*self.ov);
            }
            self.unhook();
            for h in [self.pipe, self.event, self.helper_mutex] {
                if !h.is_null() && h != INVALID_HANDLE_VALUE {
                    // SAFETY: closes handles this helper created and still owns.
                    unsafe { CloseHandle(h) };
                }
            }
        }
    }

    /// Watch the game process; when it ends, report `game_exited` and exit (design failure table).
    fn spawn_game_watcher(pid: u32) {
        std::thread::spawn(move || {
            // SAFETY: opens the game for SYNCHRONIZE and waits for it to exit.
            unsafe {
                let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
                if h.is_null() {
                    return;
                }
                WaitForSingleObject(h, INFINITE);
                CloseHandle(h);
            }
            emit(&Event::GameExited);
            std::process::exit(0);
        });
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
            let name = wide(&pipe_name(pid));
            // SAFETY: one overlapped, first-instance, message-mode, local-only duplex pipe.
            self.pipe = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    (HEADER_LEN + MAX_PAYLOAD) as u32,
                    (HEADER_LEN + MAX_PAYLOAD) as u32,
                    0,
                    std::ptr::null(),
                )
            };
            if self.pipe == INVALID_HANDLE_VALUE {
                // SAFETY: no arguments.
                return Err(format!("CreateNamedPipeW: {}", unsafe { GetLastError() }));
            }
            // SAFETY: a manual-reset, initially unsignaled event for overlapped I/O.
            self.event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if self.event.is_null() {
                return Err("CreateEventW failed".into());
            }
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
            self.accept_hello(timeout_ms)
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

    pub fn main() -> i32 {
        let mut args = std::env::args().skip(1);
        let exe = args.next().unwrap_or_default();
        let mut bootstrap = String::new();
        let mut force_fallback = false;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--bootstrap" => bootstrap = args.next().unwrap_or_default(),
                "--fallback" => force_fallback = true,
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

        let mut w = RealWin32::new(exe, bootstrap);
        // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
        // do not block for two minutes; unset in production, the defaults stand.
        let env_ms = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let defaults = Config::default();
        let cfg = Config {
            force_fallback,
            window_timeout_ms: env_ms("UW_WINDOW_TIMEOUT_MS", defaults.window_timeout_ms),
            hello_timeout_ms: env_ms("UW_HELLO_TIMEOUT_MS", defaults.hello_timeout_ms),
            window_poll_ms: defaults.window_poll_ms,
        };
        let outcome = run(&mut w, &cfg);
        emit(&outcome.to_event());
        match outcome {
            Outcome::Connected { .. } => {
                w.relay();
                0
            }
            // The game ended before the bootstrap connected: the same report, and the same exit
            // code, as the post-connect watcher's `game_exited`.
            Outcome::GameExited => 0,
            other => {
                eprintln!("[uw-helper64] {other:?}");
                1
            }
        }
    }
}
