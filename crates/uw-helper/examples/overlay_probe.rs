//! A scripted overlay-pipe client for the helper's Wine tests (`tests/wine_e2e.rs`).
//!
//! `overlay_probe <mode> [ms]` finds `uw-test-game.exe`, connects to its overlay pipe as the
//! overlay would, and prints one line per step on stdout so a test can follow it. Modes:
//! - `bad-first`: send a message that is not `overlay_hello` first; expects to be dropped.
//! - `bad-version`: send an `overlay_hello` with an unsupported state layout; expects a drop.
//! - `ok-then-bad` / `oversize`: say hello, then send garbage (or an oversized message);
//!   expects a drop, then reconnects with a good hello to show the helper listens again.
//! - `hold [ms]`: say hello, hold the state lock for `ms` (default 3000) with `applied_seq`
//!   set to 42 and `state_applied` rung, then act as the game side: answer each `bell-game`
//!   by applying the current `seq` and ringing `state_applied`, printing `bell <bits> seq <n>`.
//! - `hold-info-lock [ms]`: without touching the overlay pipe, hold the game's data channel 1
//!   info lock for `ms` (default 3000), ringing its info bell every 200 ms meanwhile; prints
//!   `locked`, then `released` (design 2b: a busy lock).
//! - `break-info-magic`: without touching the overlay pipe, break the magic of the game's data
//!   channel 1 info store, under its lock, as a producer gone wrong would; prints `broken`.
//!
//! Runs under Wine in the game's prefix. It does nothing on other platforms.

#[cfg(not(windows))]
fn main() {
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::time::{Duration, Instant};

    use uw_game_channel::objects::{LOCK_TIMEOUT_MS, Objects};
    use uw_pipe_proto::bells::to_host;
    use uw_pipe_proto::data::DEFAULT_CHANNEL;
    use uw_pipe_proto::names::{DataObject, data_name, overlay_pipe_name};
    use uw_pipe_proto::pipe::{OverlayMessage, encode_overlay};
    use uw_pipe_proto::state;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_ABANDONED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Memory::{
        FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    };
    use windows_sys::Win32::System::Pipes::{PIPE_READMODE_MESSAGE, SetNamedPipeHandleState};
    use windows_sys::Win32::System::Threading::{
        EVENT_MODIFY_STATE, MUTEX_MODIFY_STATE, OpenEventW, OpenMutexW, ReleaseMutex,
        SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
    };

    fn say(s: &str) {
        use std::io::Write;
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn game_pid() -> Option<u32> {
        // SAFETY: the snapshot is closed before returning; `e` is zeroed with `dwSize` set, as `Process32FirstW`/`NextW` require.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            let mut found = None;
            while ok {
                let end = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                let s = String::from_utf16_lossy(&e.szExeFile[..end]);
                if s.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&s)
                    .eq_ignore_ascii_case("uw-test-game.exe")
                {
                    found = Some(e.th32ProcessID);
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            found
        }
    }

    struct Pipe(HANDLE);
    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: closes the pipe handle this value owns, exactly once (`Pipe` is only built from a valid handle).
            unsafe { CloseHandle(self.0) };
        }
    }

    fn connect(pid: u32) -> Pipe {
        let name = wide(&overlay_pipe_name(pid));
        let start = Instant::now();
        loop {
            // SAFETY: `name` is NUL-terminated and outlives the call; no security attributes, no template.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                let mode = PIPE_READMODE_MESSAGE;
                // SAFETY: `h` is the pipe just opened; `mode` lives across the call; the other two may be null.
                unsafe { SetNamedPipeHandleState(h, &mode, std::ptr::null(), std::ptr::null()) };
                return Pipe(h);
            }
            if start.elapsed() > Duration::from_secs(30) {
                // SAFETY: no arguments.
                say(&format!("connect failed {}", unsafe { GetLastError() }));
                std::process::exit(5);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn write(p: &Pipe, b: &[u8]) -> bool {
        let mut n = 0u32;
        // SAFETY: `p.0` is an open pipe handle; `b` is valid for `b.len()` bytes; `n` outlives the call; no overlapped I/O.
        unsafe {
            WriteFile(
                p.0,
                b.as_ptr(),
                b.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            ) != 0
        }
    }

    /// One message, or the error code.
    fn read(p: &Pipe) -> Result<Vec<u8>, u32> {
        let mut buf = vec![0u8; 8192];
        let mut n = 0u32;
        // SAFETY: `p.0` is an open pipe handle; `buf` is valid for `buf.len()` bytes; `n` outlives the call; no overlapped I/O.
        let ok = unsafe {
            ReadFile(
                p.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: no arguments.
            return Err(unsafe { GetLastError() });
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    fn hello(layout: u16) -> Vec<u8> {
        encode_overlay(&OverlayMessage::Hello {
            module_version: 77,
            state_layout: layout,
        })
    }

    fn expect_drop(p: &Pipe) -> i32 {
        match read(p) {
            Err(e) => {
                say(&format!("dropped {e}"));
                0
            }
            Ok(m) => {
                say(&format!("read {} bytes", m.len()));
                3
            }
        }
    }

    /// Hold the game's data channel 1 info lock for `ms`, ringing its info bell every 200 ms, as a
    /// producer that holds the lock too long would.
    fn hold_info_lock(pid: u32, ms: u64) -> i32 {
        let lock = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoLock));
        let bell = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoBell));
        // SAFETY: opens the game's named mutex and event; both names are NUL-terminated.
        let (l, b) = unsafe {
            (
                OpenMutexW(
                    SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                    0,
                    lock.as_ptr(),
                ),
                OpenEventW(EVENT_MODIFY_STATE, 0, bell.as_ptr()),
            )
        };
        if l.is_null() || b.is_null() {
            // SAFETY: closes the handles that did open; each non-null handle is valid and owned here.
            unsafe {
                if !l.is_null() {
                    CloseHandle(l);
                }
                if !b.is_null() {
                    CloseHandle(b);
                }
            }
            say("no data channel");
            return 7;
        }
        // SAFETY: waits on the mutex handle opened above.
        let waited = unsafe { WaitForSingleObject(l, 5000) };
        if !matches!(waited, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            // SAFETY: closes the handles opened above.
            unsafe {
                CloseHandle(l);
                CloseHandle(b);
            }
            say("lock busy");
            return 8;
        }
        say("locked");
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(ms) {
            // SAFETY: signals the event handle opened above.
            unsafe { SetEvent(b) };
            std::thread::sleep(Duration::from_millis(200));
        }
        // SAFETY: releases the mutex this thread acquired above, then closes both handles.
        unsafe {
            ReleaseMutex(l);
            CloseHandle(l);
            CloseHandle(b);
        }
        say("released");
        0
    }

    /// Break the first byte of the game's data channel 1 info store, under the info lock.
    fn break_info_magic(pid: u32) -> i32 {
        let lock = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoLock));
        let info = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::Info));
        // SAFETY: opens the game's named mutex and mapping; both names are NUL-terminated.
        let (l, m) = unsafe {
            (
                OpenMutexW(
                    SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                    0,
                    lock.as_ptr(),
                ),
                OpenFileMappingW(FILE_MAP_WRITE, 0, info.as_ptr()),
            )
        };
        if l.is_null() || m.is_null() {
            // SAFETY: closes the handles that did open; each non-null handle is valid and owned here.
            unsafe {
                if !l.is_null() {
                    CloseHandle(l);
                }
                if !m.is_null() {
                    CloseHandle(m);
                }
            }
            say("no data channel");
            return 7;
        }
        // SAFETY: waits on the mutex handle opened above.
        let waited = unsafe { WaitForSingleObject(l, 5000) };
        if !matches!(waited, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            // SAFETY: closes the handles opened above.
            unsafe {
                CloseHandle(l);
                CloseHandle(m);
            }
            say("lock busy");
            return 8;
        }
        // SAFETY: maps the store for writing and, if that worked, writes its first byte while the
        // lock is held; then unmaps it, releases the lock and closes both handles.
        let mapped = unsafe {
            let view = MapViewOfFile(m, FILE_MAP_WRITE, 0, 0, 0);
            let mapped = !view.Value.is_null();
            if mapped {
                *view.Value.cast::<u8>() = b'X';
                UnmapViewOfFile(view);
            }
            ReleaseMutex(l);
            CloseHandle(l);
            CloseHandle(m);
            mapped
        };
        if !mapped {
            say("map failed");
            return 9;
        }
        say("broken");
        0
    }

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = args.first().cloned().unwrap_or_default();
        let ms: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(3000);
        let Some(pid) = game_pid() else {
            say("no game");
            return 4;
        };
        if mode == "hold-info-lock" {
            return hold_info_lock(pid, ms);
        }
        if mode == "break-info-magic" {
            return break_info_magic(pid);
        }
        let p = connect(pid);
        match mode.as_str() {
            "bad-first" => {
                write(&p, &encode_overlay(&OverlayMessage::Ping { nonce: 1 }));
                expect_drop(&p)
            }
            "bad-version" => {
                write(&p, &hello(2));
                expect_drop(&p)
            }
            "ok-then-bad" | "oversize" => {
                write(&p, &hello(1));
                std::thread::sleep(Duration::from_millis(500));
                if mode == "oversize" {
                    write(&p, &vec![0u8; 5000]);
                } else {
                    write(&p, b"not a frame at all");
                }
                let r = expect_drop(&p);
                drop(p);
                let p2 = connect(pid);
                write(&p2, &hello(1));
                say("reconnected");
                std::thread::sleep(Duration::from_secs(30));
                r
            }
            "hold" => {
                write(&p, &hello(1));
                let o = match Objects::open(pid) {
                    Ok(o) => o,
                    Err(e) => {
                        say(&format!("open failed {e}"));
                        return 6;
                    }
                };
                {
                    let mut g = o.lock(LOCK_TIMEOUT_MS).expect("lock");
                    state::set_applied_seq(g.bytes_mut(), 42);
                    o.ring_host(to_host::STATE_APPLIED);
                    say("locked");
                    std::thread::sleep(Duration::from_millis(ms));
                }
                say("released");
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(60) {
                    // SAFETY: waits on the bell-game event handle `o` owns.
                    if unsafe { WaitForSingleObject(o.bell_game(), 300) } != WAIT_OBJECT_0 {
                        continue;
                    }
                    let bits = o.take_to_game();
                    let seq = {
                        let mut g = o.lock(LOCK_TIMEOUT_MS).expect("lock");
                        let seq = state::read(g.bytes()).seq;
                        state::set_applied_seq(g.bytes_mut(), seq);
                        seq
                    };
                    o.ring_host(to_host::STATE_APPLIED);
                    say(&format!("bell {bits} seq {seq}"));
                }
                0
            }
            other => {
                say(&format!("unknown mode {other}"));
                2
            }
        }
    }
}
