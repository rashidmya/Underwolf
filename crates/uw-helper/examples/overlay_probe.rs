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
    use uw_pipe_proto::names::overlay_pipe_name;
    use uw_pipe_proto::pipe::{OverlayMessage, encode_overlay};
    use uw_pipe_proto::state;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Pipes::{PIPE_READMODE_MESSAGE, SetNamedPipeHandleState};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

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
            unsafe { CloseHandle(self.0) };
        }
    }

    fn connect(pid: u32) -> Pipe {
        let name = wide(&overlay_pipe_name(pid));
        let start = Instant::now();
        loop {
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
                unsafe { SetNamedPipeHandleState(h, &mode, std::ptr::null(), std::ptr::null()) };
                return Pipe(h);
            }
            if start.elapsed() > Duration::from_secs(30) {
                say(&format!("connect failed {}", unsafe { GetLastError() }));
                std::process::exit(5);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn write(p: &Pipe, b: &[u8]) -> bool {
        let mut n = 0u32;
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

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mode = args.first().cloned().unwrap_or_default();
        let ms: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(3000);
        let Some(pid) = game_pid() else {
            say("no game");
            return 4;
        };
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
