# Injection (Milestone 1, Plan 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load Underwolf's in-game code into a running game under Proton without remote-thread injection: a helper process sets a thread-targeted message hook that makes the game load a pinned bootstrap DLL, which connects to the helper over a named pipe and, on command, loads and starts the overlay DLL.

**Architecture:** Six new crates under `crates/` (added to the workspace; nothing depends on `spikes/`). `uw-inject` (Linux) finds the game and its Wine session and starts `uw-helper64.exe` inside it. The helper finds the game's UI thread, loads `uw_bootstrap64.dll` into itself, calls `SetWindowsHookExW(WH_GETMESSAGE, …)` (with a `WH_CALLWNDPROC` fallback) on that thread, and serves a named pipe. Wine loads the bootstrap into the game; the bootstrap pins itself, connects back over the pipe, and on `load_overlay` loads the overlay DLL and calls `uw_overlay_start`. The helper↔bootstrap pipe is fixed-layout binary (`uw-pipe-proto`); the Linux↔helper channel is one JSON object per line (temporary; plan 2 replaces it). All decision logic that can be is portable and unit-tested on Linux behind a `Win32` trait with a fake; the FFI is proved under plain Wine and then in Deadlock by the owner.

**Tech Stack:** Rust (edition 2024, rustc 1.96.1). Targets `x86_64-unknown-linux-gnu` (inject, all unit tests) and `x86_64-pc-windows-gnu` (helper, bootstrap, test overlay, test game). `windows-sys = "0.61.2"` for raw Win32 FFI (no COM). `serde`/`serde_json` for the JSON-lines channel. Tests: `cargo test` (Linux unit) and ignored Wine end-to-end tests that need system Wine 11.20 at `/usr/bin/wine`. mingw (`x86_64-w64-mingw32-gcc`) links the Windows artifacts.

---

## Why these facts (looked up 2026-10-22)

- **`windows-sys` 0.61.2 feature names and signatures** come from the crate source in `~/.cargo/registry` (the released crate), cross-checked against Context7 `/microsoft/windows-rs`. Exact feature flags and the signatures used below (`SetWindowsHookExW`, `CreateNamedPipeW`, `CreateFileW`, `CreateMutexW`/`OpenMutexW`, `GetModuleHandleExW`, Toolhelp, window APIs) are quoted in the tasks that use them. `HOOKPROC = Option<unsafe extern "system" fn(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT>`; `WH_GETMESSAGE = 3`, `WH_CALLWNDPROC = 4`; `GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS = 4`, `GET_MODULE_HANDLE_EX_FLAG_PIN = 1`. Handles (`HANDLE`, `HWND`, `HMODULE`, `HINSTANCE`) are all `*mut c_void`; `windows_sys::core::BOOL = i32` with `TRUE = 1`/`FALSE = 0`; `w!`/`s!` wide/ansi literal macros live in `windows_sys::core`.
- **`panic = "unwind"`**: the workspace root `Cargo.toml` sets no `[profile]`, so release builds unwind by default; `std::panic::catch_unwind` therefore works in the bootstrap and overlay without any per-crate profile override (per-crate `[profile.*]` is ignored outside the workspace root). Verified by building a `cdylib` with `catch_unwind` for `x86_64-pc-windows-gnu` and running an exe that calls Win32 under `WINEPREFIX`.
- **Wine loads a thread-hook DLL into the target.** Not documented; this plan's Task 1 proves it under plain Wine and then in Deadlock. If neither `WH_GETMESSAGE` nor `WH_CALLWNDPROC` loads the bootstrap, stop and report to the owner (remote-thread injection is ruled out by the spec).

## File map

New crates in `crates/` (workspace `members` gains `"crates/*"`). `spikes/` is untouched.

| Path | Builds | Responsibility |
|---|---|---|
| `crates/uw-pipe-proto/src/lib.rs` | library (Linux + Windows) | Declares `pub mod pipe`, `pub mod jsonl`, `pub mod names`. |
| `crates/uw-pipe-proto/src/pipe.rs` | (same crate) | Fixed-layout binary header + payloads, limits, encode/decode, every rejection. |
| `crates/uw-pipe-proto/src/jsonl.rs` | (same crate) | serde `Command`/`Event` JSON line types. |
| `crates/uw-pipe-proto/src/names.rs` | (same crate) | Per-game pipe and mutex names (shared by helper and bootstrap). |
| `crates/uw-pipe-proto/tests/pipe.rs` | test (Linux) | Round trips and every rejection. |
| `crates/uw-pipe-proto/tests/jsonl.rs` | test (Linux) | JSON line parse/emit. |
| `crates/uw-inject/src/lib.rs` | Linux library | Re-exports discovery/path helpers and `pub mod driver` (portable JSON-lines driving). |
| `crates/uw-inject/src/discovery.rs` | Linux library | Ported `/proc` + `config_info` + `wine_env` + `server_socket` + `to_wine_path` + `exe_matches` + `find_pids` + `GameSession`. |
| `crates/uw-inject/src/bin/uw-inject.rs` | `uw-inject` CLI (Linux) | Find game, spawn helper in its Wine session, drive JSON lines. |
| `crates/uw-inject/tests/discovery.rs` | test (Linux) | Path/config/env/exe/`find_pids` parsing, `GameError` codes, the `Driver`. |
| `crates/uw-inject/tests/cli.rs` | test (Linux) | `helper_command` construction. |
| `crates/uw-inject/tests/wine_e2e.rs` | test (Linux, ignored) | Full `uw-inject` → helper → bootstrap → overlay chain under plain Wine. |
| `crates/uw-helper/src/lib.rs` | library (Linux + Windows) | Declares `pub mod decide` (the portable `Win32` trait + state machine); the shared names live in `uw-pipe-proto::names`. |
| `crates/uw-helper/src/decide.rs` | library | Wait/hook/fallback/skip-on-marker/timeouts/version decision logic. |
| `crates/uw-helper/src/bin/uw-helper64.rs` | `uw-helper64.exe` | `cfg(windows)` real `Win32` impl (window/thread lookup, hook, pipe server, mutexes) + JSON lines on stdin/stdout. |
| `crates/uw-helper/tests/decide.rs` | test (Linux) | Every decision branch against a fake `Win32`. |
| `crates/uw-helper/tests/wine_e2e.rs` | test (Linux, ignored) | The design's helper-level Wine scenarios. |
| `crates/uw-bootstrap/src/lib.rs` | `uw_bootstrap64.dll` | Thin crate root: `pub mod state` (portable) + `#[cfg(windows)] mod win`. |
| `crates/uw-bootstrap/src/win.rs` | (same crate) | `cfg(windows)` hook proc, pin, worker thread, marker mutex, pipe connect, overlay load. |
| `crates/uw-bootstrap/src/state.rs` | (same crate) | Portable overlay-load decision (`ok`/`already_loaded`/`already_loaded_other`). |
| `crates/uw-bootstrap/tests/state.rs` | test (Linux) | The overlay-load decision. |
| `crates/uw-test-overlay/src/lib.rs` | test DLL | `uw_overlay_start` writes `%TEMP%\uw-test-overlay-<pid>.txt` (catching its own panics). The `no_start_export` test loads `uw_bootstrap64.dll` (a real DLL without that export), so no separate no-export build is needed. |
| `crates/uw-test-game/src/main.rs` | test exe | One visible top-level window + standard message loop; `--no-window` mode. |

**Shared names** (one definition, in `uw-pipe-proto::names`, used by both the helper and the bootstrap): pipe `\\.\pipe\underwolf-<pid>`, helper mutex `Local\underwolf-helper-<pid>`, marker mutex `Local\underwolf-bootstrap-<pid>`, where `<pid>` is the game's **Windows (Wine)** process id. `PROTOCOL_VERSION = 1` (in `uw-pipe-proto::pipe`). Bootstrap hook export name `uw_bootstrap_hook`. Overlay export `uw_overlay_start`. `StartParams { size: u32, version: u32, pid: u32 }`, `START_PARAMS_VERSION = 1`. (`uw-helper` is created minimal in Task 1, before `uw-pipe-proto` exists, and gains its `uw-pipe-proto` dependency in Task 4.)

---

## Task 1: Prove the hook mechanism under Wine, then in Deadlock (GATE)

This is the design's "Testing", last paragraph. It builds the three crates minimally — `uw-test-game`, `uw-bootstrap`, `uw-helper` — so later tasks grow them; nothing here is thrown away. The proof: the helper sets a thread hook on the game's UI thread, and the bootstrap's hook procedure runs **inside the game process** and writes a proof file naming the game's pid. **If neither `WH_GETMESSAGE` nor `WH_CALLWNDPROC` produces the proof file, stop and report to the owner.**

**Files:**
- Create: `crates/uw-test-game/Cargo.toml`, `crates/uw-test-game/src/main.rs`
- Create: `crates/uw-bootstrap/Cargo.toml`, `crates/uw-bootstrap/src/lib.rs`, `crates/uw-bootstrap/src/win.rs`
- Create: `crates/uw-helper/Cargo.toml`, `crates/uw-helper/src/lib.rs`, `crates/uw-helper/src/bin/uw-helper64.rs`
- Create: `crates/uw-helper/tests/wine_e2e.rs`
- Modify: `Cargo.toml` (workspace `members`)

- [ ] **Step 1: Add `crates/*` to the workspace**

Edit the root `Cargo.toml` `members` line:

```toml
[workspace]
resolver = "3"
members = ["spikes/*", "crates/*"]
exclude = ["spikes/osr-host", "spikes/bench"]
```

- [ ] **Step 2: Create the minimal test game**

`crates/uw-test-game/Cargo.toml`:

```toml
[package]
name = "uw-test-game"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# A stand-in game for the Wine end-to-end tests: one visible top-level window and a standard
# message loop, so the helper's hook has a UI thread to target. `--no-window` sleeps instead
# (for the `no_window` test). Built for x86_64-pc-windows-gnu.
[[bin]]
name = "uw-test-game"
path = "src/main.rs"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Gdi",
    "Win32_UI_WindowsAndMessaging",
    "Win32_System_LibraryLoader",
] }
```

`crates/uw-test-game/src/main.rs`:

```rust
//! A stand-in game for the Wine end-to-end tests. `uw-test-game` opens one visible top-level
//! window (no owner) and runs a standard `GetMessage`/`TranslateMessage`/`DispatchMessage` loop,
//! so a thread-targeted `WH_GETMESSAGE`/`WH_CALLWNDPROC` hook has a UI thread to run on.
//! `uw-test-game --no-window` sleeps with no window (for the `no_window` scenario).

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-test-game is a Windows program; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--no-window") {
        // No window: the helper must give up with `no_window`.
        std::thread::sleep(std::time::Duration::from_secs(300));
        return;
    }
    win::run();
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
        RegisterClassW, ShowWindow, TranslateMessage, CW_USEDEFAULT, MSG, SW_SHOW, WM_DESTROY,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows_sys::core::w;

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
            let _ = (msg.pt, LPARAM::default(), std::ptr::null::<c_void>());
        }
    }
}
```

- [ ] **Step 3: Create the minimal bootstrap DLL**

`crates/uw-bootstrap/Cargo.toml`:

```toml
[package]
name = "uw-bootstrap"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The pinned in-game bootstrap: a thread-hook procedure Wine loads into the game. On the first
# call it pins itself and starts one worker thread, then always `CallNextHookEx`. `DllMain` does
# nothing (it also runs in the helper). Built as `uw_bootstrap64.dll` for x86_64-pc-windows-gnu.
# Task 5 grows the worker to connect the pipe and load the overlay. `lib` (rlib) is needed so the
# crate's `state` unit tests can link it on Linux; `cdylib` is the DLL.
[lib]
name = "uw_bootstrap64"
crate-type = ["lib", "cdylib"]

[target.'cfg(windows)'.dependencies]
# `CreateFileW` needs `Win32_Security` and `WriteFile` needs `Win32_System_IO` (windows-sys gates
# each function by the features of the types in its signature), so both are required even though
# the bootstrap passes null/ignored arguments for them.
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_SystemServices",
    "Win32_System_Threading",
    "Win32_UI_WindowsAndMessaging",
] }
```

`crates/uw-bootstrap/src/lib.rs` (thin; the crate root is **not** `#![cfg(windows)]` so Task 5 can add a portable, Linux-tested `state` module):

```rust
//! The pinned in-game bootstrap DLL (`uw_bootstrap64.dll`). The helper loads it with a
//! `SetWindowsHookExW` thread hook; Wine then loads it into the game and calls `uw_bootstrap_hook`
//! there. On the first call only, it pins itself (so unhooking cannot unload it) and starts one
//! worker thread; every call ends in `CallNextHookEx`. `DllMain` does nothing, because this DLL is
//! also loaded into the helper (where `SetWindowsHookExW` requires a DLL-resident procedure).
//!
//! In this task the worker only proves the mechanism: it writes `%TEMP%\uw-bootstrap-proof-<pid>.txt`
//! with the game's pid. Task 5 adds `pub mod state` (portable) and grows `win` into the pipe
//! connect + overlay load.

#[cfg(windows)]
mod win;
```

`crates/uw-bootstrap/src/win.rs` (the `#[unsafe(no_mangle)]` exports are emitted from this private module into the cdylib):

```rust
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
```

- [ ] **Step 4: Create the minimal helper**

`crates/uw-helper/Cargo.toml`:

```toml
[package]
name = "uw-helper"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The Wine-side helper (`uw-helper64.exe`): finds the game's UI thread and sets a thread hook that
# makes Wine load `uw_bootstrap64.dll` into the game, then (Task 6) serves the named pipe. Task 1
# is the minimal proof: hook, post wake-ups, wait for the bootstrap's proof file, exit. The
# portable decision logic (Task 4) and pipe wiring (Task 6) are added to `src/lib.rs` and the bin.
[lib]
name = "uw_helper"
path = "src/lib.rs"

[[bin]]
name = "uw-helper64"
path = "src/bin/uw-helper64.rs"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Gdi",
    "Win32_Storage_FileSystem",
    "Win32_UI_WindowsAndMessaging",
    "Win32_System_LibraryLoader",
    "Win32_System_Threading",
    "Win32_System_Diagnostics_ToolHelp",
] }
```

`crates/uw-helper/src/lib.rs` (minimal for now; Task 4 adds `pub mod decide`). `uw-pipe-proto` does not exist yet, so this crate has no dependency on it until Task 4:

```rust
//! The portable half of the helper. Task 4 adds `pub mod decide` (the `Win32` trait and the
//! wait/hook/fallback/timeout state machine). For Task 1 the proof lives entirely in the bin, so
//! the library is intentionally empty.
```

`crates/uw-helper/src/bin/uw-helper64.rs`:

```rust
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

    use windows_sys::Win32::Foundation::{HWND, LPARAM, MAX_PATH, RECT};
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
        // SAFETY: a process snapshot walked with correctly sized PROCESSENTRY32W.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                if e.th32ProcessID != GetCurrentProcessId() && basename_eq(&e.szExeFile, exe) {
                    return Some(e.th32ProcessID);
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
        }
        None
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
        let mut buf = [0u16; MAX_PATH as usize];
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
            if let Some(pid) = find_game(&exe) {
                if let Some((t, h)) = ui_thread(pid) {
                    break (pid, t, h);
                }
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
        // SAFETY: transmute the resolved export to the hook procedure type.
        let hookproc: HOOKPROC =
            Some(unsafe { std::mem::transmute::<_, unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>(proc_addr) });

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
```

- [ ] **Step 5: Write the Wine proof test**

`crates/uw-helper/tests/wine_e2e.rs`:

```rust
#![cfg(unix)]
//! End-to-end proof of the hook mechanism under plain Wine (system `/usr/bin/wine`, not Proton).
//! A `uw-test-game` window runs in a scratch prefix; `uw-helper64.exe` runs in the same prefix
//! (same wineserver), hooks the game's UI thread, and the bootstrap writes its proof file.
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-helper
//!   xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE` overrides the `wine` binary (default `/usr/bin/wine`); `UW_WINESERVER` the
//! `wineserver` (default `/usr/bin/wineserver`). The scratch prefix goes under the system temp.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}

fn win_artifact(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release")
        .join(name);
    assert!(p.exists(), "missing {} (build the Windows artifacts first)", p.display());
    p
}

fn to_wine_path(p: &Path) -> String {
    format!("Z:{}", p.to_string_lossy().replace('/', "\\"))
}

struct Prefix {
    dir: PathBuf,
    game: Option<Child>,
}

/// Wait up to `d` for `child` to exit, then kill it. Every teardown wait is bounded, so a stuck
/// Wine process fails or slows a test instead of hanging the whole suite.
fn wait_bounded(child: &mut Child, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for Prefix {
    fn drop(&mut self) {
        if let Some(mut g) = self.game.take() {
            let _ = g.kill();
            wait_bounded(&mut g, Duration::from_secs(10));
        }
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.dir).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap_or(&self.dir));
    }
}

fn setup(tag: &str) -> Prefix {
    let dir = std::env::temp_dir()
        .join(format!("uw-inject-{tag}-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&dir).unwrap();
    Prefix { dir, game: None }
}

/// Runs a Windows program in the game's scratch prefix (same wineserver as the game).
/// `WINEDLLOVERRIDES=mscoree,mshtml=` disables the Mono/Gecko installer prompts on a fresh prefix.
fn wine_cmd(pfx: &Prefix, program: &Path) -> Command {
    let mut c = Command::new(wine());
    c.arg(program)
        .env("WINEPREFIX", &pfx.dir)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=");
    c
}

/// Files starting with `stem` in the prefix users' `%TEMP%`
/// (`drive_c/users/<user>/AppData/Local/Temp`). This is what the design means by "read it through
/// the prefix". **No recursion:** a Wine prefix has `dosdevices/z: -> /`, so a recursive walk that
/// follows directory symlinks escapes into the whole filesystem (and into every other prefix's
/// `z:`) and never finishes — which hangs the test and skips its `Drop` cleanup.
fn markers(pfx: &Prefix, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for user in std::fs::read_dir(pfx.dir.join("drive_c/users")).into_iter().flatten().flatten() {
        let temp = user.path().join("AppData/Local/Temp");
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(stem) {
                out.push(e.path());
            }
        }
    }
    out
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn hook_loads_bootstrap_into_the_game() {
    let mut pfx = setup("proof");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());

    // The first run creates the prefix (wineboot); the helper polls for the game window. Give the
    // whole proof up to 200 s.
    let out = wine_cmd(&pfx, &win_artifact("uw-helper64.exe"))
        .arg("uw-test-game.exe")
        .arg("--bootstrap")
        .arg(to_wine_path(&win_artifact("uw_bootstrap64.dll")))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output_timeout(Duration::from_secs(200));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("proof "), "helper did not prove the hook; stdout: {stdout}");
    // The bootstrap wrote its proof file into the prefix: its presence is the proof.
    assert_eq!(markers(&pfx, "uw-bootstrap-proof-").len(), 1, "expected one proof file in the prefix");
}

/// Minimal `wait_with_output` with a timeout, so a stuck Wine process fails the test (and is
/// killed) instead of hanging it.
trait WaitTimeout {
    fn wait_with_output_timeout(self, d: Duration) -> std::process::Output;
}
impl WaitTimeout for Child {
    fn wait_with_output_timeout(mut self, d: Duration) -> std::process::Output {
        let start = Instant::now();
        loop {
            if let Some(_status) = self.try_wait().unwrap() {
                return self.wait_with_output().unwrap();
            }
            if start.elapsed() >= d {
                let _ = self.kill();
                let _ = self.wait();
                panic!("helper did not exit within {d:?}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
```

- [ ] **Step 6: Build the Windows artifacts and run the proof (expect green under Wine)**

Run (under Xvfb so no game window reaches the screen, and single-threaded so only one prefix boots):
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-helper
xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
```
Expected: `hook_loads_bootstrap_into_the_game ... ok` and `test result: ok. 1 passed`. The helper's stderr shows `pid … ui thread …` and `hooked WH_GETMESSAGE thread=…`; its stdout line begins `proof `.

**If the test fails with `no_proof`**, rerun with `--fallback` (the helper flag that forces `WH_CALLWNDPROC`). **If neither hook produces the proof file under Wine, STOP: report to the owner that the Wine hook mechanism does not load the DLL into the target, per the design's gate. Do not continue.**

- [ ] **Step 7: Owner live check in Deadlock**

Ask the owner to run this from the repo root, with Deadlock running under Proton 11.0. It records the game build, joins the game's exact Wine session (prefix **and** every `WINE*SYNC` — Proton 11.0 sets `WINEFSYNC=1`, which must match the running wineserver), and checks the proof file and that the game survives:

```bash
# From the repo root, Deadlock running under Proton 11.0:
grep ClientVersion ~/.local/share/Steam/steamapps/common/Deadlock/game/citadel/steam.inf
#   expect one line, e.g. ClientVersion=6813 (record it with the result)
GAME=$(pgrep -x deadlock.exe); echo "$GAME"        # the Wine game process (comm); expect one pid
export $(tr '\0' '\n' </proc/$GAME/environ | grep -E '^(WINEPREFIX|WINE[A-Z]*SYNC)=')
echo "$WINEPREFIX $WINEFSYNC"                      # expect …/compatdata/1422450/pfx/ 1
cargo build --release --target x86_64-pc-windows-gnu -p uw-bootstrap -p uw-helper
WINEDEBUG=-all "$HOME/.local/share/Steam/steamapps/common/Proton 11.0/files/bin/wine" \
    target/x86_64-pc-windows-gnu/release/uw-helper64.exe deadlock.exe \
    --bootstrap "Z:$(pwd | sed 's#/#\\#g')\\target\\x86_64-pc-windows-gnu\\release\\uw_bootstrap64.dll"
#   expect stdout: proof C:\users\steamuser\AppData\Local\Temp\uw-bootstrap-proof-<pid>.txt
#   and stderr: [uw-helper64] hooked WH_GETMESSAGE|WH_CALLWNDPROC thread=…
ls "$WINEPREFIX"drive_c/users/steamuser/AppData/Local/Temp/uw-bootstrap-proof-*.txt  # expect 1 file
pgrep -x deadlock.exe                              # expect the same pid: the game still runs
```
Expected: the helper prints the `proof …` line and exits 0, the proof file exists, the stderr line names which hook fired, and **Deadlock keeps running normally**. If it prints `no_proof`, rerun with `--fallback` appended (its wake-ups — fixed per I1 — make `WH_CALLWNDPROC` fire). **If both fail, STOP and report to the owner.**

> Note on "nothing thrown away": Task 6 rewrites this minimal `uw-helper64` bin into the full
> `RealWin32` helper and deletes this proof test, since the real helper serves the pipe instead of
> writing a proof file. That is the one place Task 1 code is replaced rather than grown; the hook
> logic itself (find game → find UI thread → load bootstrap into self → `SetWindowsHookExW` → wake)
> moves into `RealWin32` intact.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/uw-test-game crates/uw-bootstrap crates/uw-helper
git commit -m "injection: prove the thread-hook bootstrap load under Wine"
```

---

## Task 2: `uw-pipe-proto` — the pipe and JSON-lines protocols

The fixed-layout binary pipe messages (helper ↔ bootstrap) and the JSON-lines channel (Linux ↔ helper). Pure, portable, unit-tested on Linux. Plan 2 extends this crate; it does not replace it.

**Files:**
- Create: `crates/uw-pipe-proto/Cargo.toml`, `crates/uw-pipe-proto/src/lib.rs`, `crates/uw-pipe-proto/src/pipe.rs`, `crates/uw-pipe-proto/src/jsonl.rs`
- Test: `crates/uw-pipe-proto/tests/pipe.rs`, `crates/uw-pipe-proto/tests/jsonl.rs`

- [ ] **Step 1: Create the crate**

`crates/uw-pipe-proto/Cargo.toml`:

```toml
[package]
name = "uw-pipe-proto"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The injection channel protocols: `pipe` (fixed-layout binary, helper <-> bootstrap, spec 6.3)
# and `jsonl` (one JSON object per line, Linux <-> helper, temporary until plan 2's relay). Pure
# and portable; builds on Linux and Windows.
[lib]
name = "uw_pipe_proto"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`crates/uw-pipe-proto/src/lib.rs`:

```rust
//! Underwolf injection protocols. `pipe` is the fixed-layout binary channel between the Wine-side
//! helper and the in-game bootstrap (spec 6.3); `jsonl` is the temporary one-object-per-line
//! channel between the Linux host and the helper (plan 2 replaces it with a binary relay);
//! `names` is the per-game pipe and mutex names both sides must agree on.

pub mod names;
pub mod pipe;
```

(`pub mod jsonl;` is added in Step 6, together with `jsonl.rs`, so Step 4 compiles.)

`crates/uw-pipe-proto/src/names.rs`:

```rust
//! Per-game object names. `<pid>` is the game's Windows (Wine) process id in every name (design
//! §3), so the helper (which knows the pid from Toolhelp) and the bootstrap (which reads its own
//! `GetCurrentProcessId`) build the same strings.

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
pub fn helper_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-helper-{pid}")
}
pub fn marker_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-bootstrap-{pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
    }
}
```

- [ ] **Step 2: Write the binary-protocol tests first**

(Throughout this plan, a "write the tests first" step is the TDD red step: the test is written before the code it calls, so running it now fails to compile — that is the expected red state. The step right after it adds the implementation and runs the tests green.)

`crates/uw-pipe-proto/tests/pipe.rs`:

```rust
use uw_pipe_proto::pipe::{
    decode, encode, Message, ProtoError, StatusCode, MAX_PATH_UNITS, MAX_PAYLOAD,
    MAX_STATUS_BYTES, PROTOCOL_VERSION,
};

fn roundtrip(m: &Message) {
    let bytes = encode(m);
    assert_eq!(&decode(&bytes).unwrap(), m, "round trip");
}

#[test]
fn hello_round_trips() {
    roundtrip(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 7,
        bitness: 64,
        overlay_loaded: true,
        overlay_path: r"Z:\games\overlay.dll".into(),
    });
    roundtrip(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 7,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
}

#[test]
fn load_overlay_round_trips() {
    roundtrip(&Message::LoadOverlay { request_id: 42, path: r"Z:\a\b\overlay.dll".into() });
}

#[test]
fn status_round_trips() {
    roundtrip(&Message::Status {
        request_id: 42,
        code: StatusCode::Ok,
        detail: 0,
        message: "started".into(),
    });
    roundtrip(&Message::Status {
        request_id: 1,
        code: StatusCode::LoadFailed,
        detail: 126,
        message: "LoadLibraryW failed".into(),
    });
}

#[test]
fn bad_magic_is_rejected() {
    let mut bytes = encode(&Message::Status {
        request_id: 1,
        code: StatusCode::Ok,
        detail: 0,
        message: String::new(),
    });
    bytes[0] = b'X';
    assert!(matches!(decode(&bytes), Err(ProtoError::BadMagic)));
}

#[test]
fn oversize_payload_is_rejected() {
    // A header claiming more than MAX_PAYLOAD payload bytes.
    let mut header = Vec::new();
    header.extend_from_slice(b"UWP1");
    header.extend_from_slice(&3u16.to_le_bytes()); // type = status
    header.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes()); // pid
    header.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
    assert!(matches!(decode(&header), Err(ProtoError::PayloadTooLarge)));
}

#[test]
fn overlong_path_length_prefix_is_rejected() {
    let long = "A".repeat(MAX_PATH_UNITS + 1);
    // decode: a load_overlay whose path length prefix exceeds the cap (encode caps by truncating).
    let mut payload = Vec::new();
    payload.extend_from_slice(&7u32.to_le_bytes()); // request id
    payload.extend_from_slice(&((MAX_PATH_UNITS + 1) as u16).to_le_bytes());
    for u in long.encode_utf16() {
        payload.extend_from_slice(&u.to_le_bytes());
    }
    let bytes = frame(2, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::StringTooLong)));
}

#[test]
fn overlong_status_message_is_rejected() {
    let payload_message = "x".repeat(MAX_STATUS_BYTES + 1);
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // request id
    payload.extend_from_slice(&0u16.to_le_bytes()); // code = ok
    payload.extend_from_slice(&0u32.to_le_bytes()); // detail
    payload.extend_from_slice(&((MAX_STATUS_BYTES + 1) as u16).to_le_bytes());
    payload.extend_from_slice(payload_message.as_bytes());
    let bytes = frame(3, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::StringTooLong)));
}

#[test]
fn unknown_type_is_reported() {
    let bytes = frame(999, &[]);
    assert!(matches!(decode(&bytes), Err(ProtoError::UnknownType(999))));
}

#[test]
fn wrong_header_version_is_rejected() {
    let mut bytes = encode(&Message::Status {
        request_id: 1,
        code: StatusCode::Ok,
        detail: 0,
        message: String::new(),
    });
    bytes[6] = 9; // header version (bytes 6..8) -> 9
    assert!(matches!(decode(&bytes), Err(ProtoError::BadVersion(9))));
}

#[test]
fn unknown_status_code_is_bad_code() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // request id
    payload.extend_from_slice(&99u16.to_le_bytes()); // code = 99 (not a StatusCode)
    payload.extend_from_slice(&0u32.to_le_bytes()); // detail
    payload.extend_from_slice(&0u16.to_le_bytes()); // empty message
    let bytes = frame(3, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::BadCode(99))));
}

/// Build a frame with a valid magic and the given type over a raw payload.
fn frame(ty: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"UWP1");
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}
```

- [ ] **Step 3: Implement the binary protocol**

`crates/uw-pipe-proto/src/pipe.rs`:

```rust
//! Fixed-layout, little-endian binary messages (spec 6.3). 16-byte header
//! (`magic`="UWP1", `type: u16`, `version: u16`, `pid: u32`, `payload_len: u32`) then a payload of
//! at most `MAX_PAYLOAD` bytes. Paths are UTF-16 with a `u16` code-unit-count prefix (<= 1024);
//! status messages are UTF-8 with a `u16` byte-count prefix (<= 256).

pub const MAGIC: [u8; 4] = *b"UWP1";
pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;
pub const MAX_PAYLOAD: usize = 4096;
pub const MAX_PATH_UNITS: usize = 1024;
pub const MAX_STATUS_BYTES: usize = 256;

const TYPE_HELLO: u16 = 1;
const TYPE_LOAD_OVERLAY: u16 = 2;
const TYPE_STATUS: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    AlreadyLoaded,
    AlreadyLoadedOther,
    LoadFailed,
    NoStartExport,
    StartFailed,
    Unsupported,
}

impl StatusCode {
    pub fn to_u16(self) -> u16 {
        match self {
            StatusCode::Ok => 0,
            StatusCode::AlreadyLoaded => 1,
            StatusCode::AlreadyLoadedOther => 2,
            StatusCode::LoadFailed => 3,
            StatusCode::NoStartExport => 4,
            StatusCode::StartFailed => 5,
            StatusCode::Unsupported => 6,
        }
    }
    pub fn from_u16(v: u16) -> Option<StatusCode> {
        Some(match v {
            0 => StatusCode::Ok,
            1 => StatusCode::AlreadyLoaded,
            2 => StatusCode::AlreadyLoadedOther,
            3 => StatusCode::LoadFailed,
            4 => StatusCode::NoStartExport,
            5 => StatusCode::StartFailed,
            6 => StatusCode::Unsupported,
            _ => return None,
        })
    }
    /// The lowercase wire name used in the JSON-lines `status` event.
    pub fn as_str(self) -> &'static str {
        match self {
            StatusCode::Ok => "ok",
            StatusCode::AlreadyLoaded => "already_loaded",
            StatusCode::AlreadyLoadedOther => "already_loaded_other",
            StatusCode::LoadFailed => "load_failed",
            StatusCode::NoStartExport => "no_start_export",
            StatusCode::StartFailed => "start_failed",
            StatusCode::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// bootstrap -> helper, always first.
    Hello {
        protocol_version: u16,
        bootstrap_version: u16,
        bitness: u16,
        overlay_loaded: bool,
        overlay_path: String,
    },
    /// helper -> bootstrap.
    LoadOverlay { request_id: u32, path: String },
    /// bootstrap -> helper.
    Status { request_id: u32, code: StatusCode, detail: u32, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    Short,
    BadMagic,
    /// The header's `version` is not `PROTOCOL_VERSION` (carries the version seen).
    BadVersion(u16),
    PayloadTooLarge,
    StringTooLong,
    BadString,
    /// A `status` with a code outside `StatusCode` (carries the raw code).
    BadCode(u16),
    UnknownType(u16),
    Truncated,
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Short => write!(f, "message shorter than the header"),
            ProtoError::BadMagic => write!(f, "bad magic"),
            ProtoError::BadVersion(v) => write!(f, "unsupported header version {v}"),
            ProtoError::PayloadTooLarge => write!(f, "payload over {MAX_PAYLOAD} bytes"),
            ProtoError::StringTooLong => write!(f, "string over its limit"),
            ProtoError::BadString => write!(f, "invalid string bytes"),
            ProtoError::BadCode(c) => write!(f, "unknown status code {c}"),
            ProtoError::UnknownType(t) => write!(f, "unknown message type {t}"),
            ProtoError::Truncated => write!(f, "payload shorter than its fields"),
        }
    }
}
impl std::error::Error for ProtoError {}

/// A path is capped at `MAX_PATH_UNITS` code units only as a last-resort floor. Callers that could
/// supply a longer path (the helper's `load_overlay`) must reject it first and report an error
/// (`path_too_long`), so a path is never silently truncated into a different, loadable path.
fn put_path(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    let n = units.len().min(MAX_PATH_UNITS);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    for u in &units[..n] {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

fn put_status_msg(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(MAX_STATUS_BYTES);
    // Do not split a UTF-8 code point at the cap.
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.extend_from_slice(&(end as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..end]);
}

/// Encode a message into a framed buffer (header + payload).
pub fn encode(m: &Message) -> Vec<u8> {
    let (ty, payload) = match m {
        Message::Hello { protocol_version, bootstrap_version, bitness, overlay_loaded, overlay_path } => {
            let mut p = Vec::new();
            p.extend_from_slice(&protocol_version.to_le_bytes());
            p.extend_from_slice(&bootstrap_version.to_le_bytes());
            p.extend_from_slice(&bitness.to_le_bytes());
            p.push(*overlay_loaded as u8);
            put_path(&mut p, overlay_path);
            (TYPE_HELLO, p)
        }
        Message::LoadOverlay { request_id, path } => {
            let mut p = Vec::new();
            p.extend_from_slice(&request_id.to_le_bytes());
            put_path(&mut p, path);
            (TYPE_LOAD_OVERLAY, p)
        }
        Message::Status { request_id, code, detail, message } => {
            let mut p = Vec::new();
            p.extend_from_slice(&request_id.to_le_bytes());
            p.extend_from_slice(&code.to_u16().to_le_bytes());
            p.extend_from_slice(&detail.to_le_bytes());
            put_status_msg(&mut p, message);
            (TYPE_STATUS, p)
        }
    };
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    // `pid` is reserved (the sender's Windows pid); 0 here, filled and checked by plan 2.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Cur<'a> {
    fn u16(&mut self) -> Result<u16, ProtoError> {
        let end = self.i + 2;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        let end = self.i + 4;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        let v = *self.b.get(self.i).ok_or(ProtoError::Truncated)?;
        self.i += 1;
        Ok(v)
    }
    fn path(&mut self) -> Result<String, ProtoError> {
        let n = self.u16()? as usize;
        if n > MAX_PATH_UNITS {
            return Err(ProtoError::StringTooLong);
        }
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            units.push(self.u16()?);
        }
        String::from_utf16(&units).map_err(|_| ProtoError::BadString)
    }
    fn status_msg(&mut self) -> Result<String, ProtoError> {
        let n = self.u16()? as usize;
        if n > MAX_STATUS_BYTES {
            return Err(ProtoError::StringTooLong);
        }
        let end = self.i + n;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        String::from_utf8(s.to_vec()).map_err(|_| ProtoError::BadString)
    }
}

/// Decode one framed message. A string-length field over its cap fails before any allocation.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtoError::Short);
    }
    if bytes[0..4] != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    let ty = u16::from_le_bytes([bytes[4], bytes[5]]);
    // A different header version means a different wire format: reject it before reading the
    // body. The helper maps `BadVersion` on a hello to `version_mismatch` (see `decide`).
    let version = u16::from_le_bytes([bytes[6], bytes[7]]);
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::BadVersion(version));
    }
    // bytes[8..12] pid — reserved, not needed to decode the body here.
    let payload_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(ProtoError::PayloadTooLarge);
    }
    let payload = bytes.get(HEADER_LEN..HEADER_LEN + payload_len).ok_or(ProtoError::Truncated)?;
    let mut c = Cur { b: payload, i: 0 };
    match ty {
        TYPE_HELLO => Ok(Message::Hello {
            protocol_version: c.u16()?,
            bootstrap_version: c.u16()?,
            bitness: c.u16()?,
            overlay_loaded: c.u8()? != 0,
            overlay_path: c.path()?,
        }),
        TYPE_LOAD_OVERLAY => Ok(Message::LoadOverlay { request_id: c.u32()?, path: c.path()? }),
        TYPE_STATUS => {
            let request_id = c.u32()?;
            let raw = c.u16()?;
            let code = StatusCode::from_u16(raw).ok_or(ProtoError::BadCode(raw))?;
            let detail = c.u32()?;
            let message = c.status_msg()?;
            Ok(Message::Status { request_id, code, detail, message })
        }
        other => Err(ProtoError::UnknownType(other)),
    }
}
```

- [ ] **Step 4: Run the binary-protocol tests (green)**

Run: `cargo test -p uw-pipe-proto --test pipe`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 5: Write the JSON-lines tests first**

`crates/uw-pipe-proto/tests/jsonl.rs`:

```rust
use uw_pipe_proto::jsonl::{Command, Event};

#[test]
fn load_overlay_command_round_trips() {
    let c = Command::LoadOverlay { path: r"Z:\games\overlay.dll".into() };
    let line = c.to_line();
    assert_eq!(line, r#"{"cmd":"load_overlay","path":"Z:\\games\\overlay.dll"}"#);
    assert_eq!(Command::parse(&line).unwrap(), c);
}

#[test]
fn events_round_trip() {
    for e in [
        Event::BootstrapConnected { overlay_loaded: false },
        Event::BootstrapConnected { overlay_loaded: true },
        Event::Status { code: "ok".into(), detail: 0, message: "overlay started".into() },
        Event::Status { code: "load_failed".into(), detail: 126, message: "LoadLibraryW failed".into() },
        Event::error("no_window"),
        Event::Error { code: "hook_failed".into(), detail: Some(1455) },
    ] {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
    }
}

#[test]
fn error_without_detail_omits_the_field() {
    assert_eq!(Event::error("no_window").to_line(), r#"{"event":"error","code":"no_window"}"#);
}

#[test]
fn status_carries_detail_and_message() {
    let line = Event::Status { code: "start_failed".into(), detail: 7, message: "nonzero".into() }.to_line();
    assert_eq!(line, r#"{"event":"status","code":"start_failed","detail":7,"message":"nonzero"}"#);
}

#[test]
fn unknown_command_is_an_error() {
    assert!(Command::parse(r#"{"cmd":"nope"}"#).is_err());
    assert!(Command::parse("not json").is_err());
}
```

- [ ] **Step 6: Implement the JSON-lines types**

Append `pub mod jsonl;` to `crates/uw-pipe-proto/src/lib.rs` (after `pub mod names;`), then create the module:

`crates/uw-pipe-proto/src/jsonl.rs`:

```rust
//! The temporary Linux-host <-> helper channel: one JSON object per line. Commands travel on the
//! helper's stdin, events on its stdout (diagnostics on stderr). Plan 2's binary relay replaces
//! this. Serialized with serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};

/// Host -> helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Load (and start) the overlay at this Windows path.
    LoadOverlay { path: String },
}

/// Helper -> host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The bootstrap connected and sent `hello` (design step 7).
    BootstrapConnected { overlay_loaded: bool },
    /// A `status` reply from the bootstrap (design step 8): the lowercase code, a numeric `detail`
    /// (a Windows error or the start function's code, 0 if none) and a human message.
    Status { code: String, detail: u32, message: String },
    /// A helper- or host-side failure (e.g. `no_window`, `hook_failed`, `game_not_found`). `detail`
    /// carries a `GetLastError` where there is one (absent otherwise).
    Error {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<u32>,
    },
}

impl Event {
    /// An error event with no numeric detail.
    pub fn error(code: &str) -> Event {
        Event::Error { code: code.to_string(), detail: None }
    }
}

impl Command {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Command serializes")
    }
    pub fn parse(line: &str) -> Result<Command, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Event serializes")
    }
    pub fn parse(line: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}
```

- [ ] **Step 7: Run the whole crate's tests (green)**

Run: `cargo test -p uw-pipe-proto`
Expected: the `names` unit test (`test result: ok. 1 passed`), the `pipe` file (`10 passed`) and the `jsonl` file (`5 passed`) all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "injection: uw-pipe-proto (binary pipe + JSON-lines protocols)"
```

---

## Task 3: `uw-inject` discovery, paths and JSON-lines driving (Linux library)

Port the spike launcher's game/session discovery and path conversion (adapted; no dependency on `spikes/`), and add the portable logic that drives the helper over JSON lines. Unit-tested on Linux. The `uw-inject` CLI binary is Task 7.

**Files:**
- Create: `crates/uw-inject/Cargo.toml`, `crates/uw-inject/src/lib.rs`, `crates/uw-inject/src/discovery.rs`, `crates/uw-inject/src/driver.rs`
- Test: `crates/uw-inject/tests/discovery.rs`

- [ ] **Step 1: Create the crate**

`crates/uw-inject/Cargo.toml`:

```toml
[package]
name = "uw-inject"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# Linux side: find a running Proton game and its Wine session, start `uw-helper64.exe` inside it,
# and drive it over JSON lines. Discovery and path conversion are ported from the injection spike
# (adapted; no dependency on spikes/). The `uw-inject` binary is the operator CLI.
[lib]
name = "uw_inject"

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

`crates/uw-inject/src/lib.rs`:

```rust
//! Linux-side injection: discover the game and its Wine session, and drive the Wine-side helper.
//! `discovery` is the ported spike launcher logic; `driver` turns the helper's JSON-lines events
//! into the commands the host sends back (load the overlay once the bootstrap connects).

pub mod driver;

#[cfg(unix)]
pub mod discovery;

/// A Unix path as Wine's `Z:` drive sees it (used for both the bootstrap and overlay paths).
pub fn to_wine_path(path: &std::path::Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// True when `name` (a bare exe name or a Windows/Unix path) names the exe `target`.
pub fn exe_matches(name: &str, target: &str) -> bool {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    base.eq_ignore_ascii_case(target)
}
```

- [ ] **Step 2: Write the discovery/driver tests first**

`crates/uw-inject/tests/discovery.rs`:

```rust
use std::path::{Path, PathBuf};

use uw_inject::driver::Driver;
use uw_inject::{exe_matches, to_wine_path};
use uw_pipe_proto::jsonl::{Command, Event};

#[cfg(unix)]
use uw_inject::discovery::{
    config_info_for_prefix, find_pids, proton_files_from_config_info, server_socket, wine_env,
    GameError, GameSession,
};

// First lines of a real `compatdata/1422450/config_info` (Proton 11.0).
const CONFIG_INFO: &str = "11.0-100
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/share/fonts/
/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files/lib/
/home/buga/.local/share/Steam
1791921018.0
";

#[cfg(unix)]
#[test]
fn proton_files_dir_comes_from_config_info() {
    assert_eq!(
        proton_files_from_config_info(CONFIG_INFO),
        Some(PathBuf::from("/home/buga/.local/share/Steam/steamapps/common/Proton 11.0/files"))
    );
    assert_eq!(proton_files_from_config_info("11.0-100\n/home/buga\n"), None);
}

#[cfg(unix)]
#[test]
fn config_info_sits_next_to_the_prefix() {
    assert_eq!(
        config_info_for_prefix(Path::new("/s/compatdata/1422450/pfx/")),
        PathBuf::from("/s/compatdata/1422450/config_info")
    );
}

#[cfg(unix)]
#[test]
fn wine_env_keeps_prefix_and_sync_settings_only() {
    let environ = b"PATH=/usr/bin\0WINEPREFIX=/s/compatdata/1422450/pfx/\0WINEFSYNC=1\0\
WINEESYNC=0\0WINENTSYNC=1\0WINEDLLPATH=/run/x\0LD_PRELOAD=/a.so\0";
    assert_eq!(
        wine_env(environ),
        vec![
            ("WINEPREFIX".to_string(), "/s/compatdata/1422450/pfx/".to_string()),
            ("WINEFSYNC".to_string(), "1".to_string()),
            ("WINEESYNC".to_string(), "0".to_string()),
            ("WINENTSYNC".to_string(), "1".to_string()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn server_socket_matches_wine_layout() {
    assert_eq!(
        server_socket(1000, 0x24, 0x17e2e),
        PathBuf::from("/tmp/.wine-1000/server-24-17e2e/socket")
    );
}

#[cfg(unix)]
#[test]
fn game_session_find_reports_a_missing_process() {
    let err = GameSession::find("uw-no-such-game.exe", None).unwrap_err();
    assert_eq!(err.code(), "game_not_found");
    assert!(matches!(err, GameError::NotRunning(_)));
    assert!(err.to_string().contains("uw-no-such-game.exe is not running"), "{err}");
}

#[cfg(unix)]
#[test]
fn game_error_codes_match_the_design() {
    assert_eq!(GameError::NotRunning("x".into()).code(), "game_not_found");
    assert_eq!(GameError::NoWineSession("x".into()).code(), "no_wine_session");
}

#[cfg(unix)]
#[test]
fn find_pids_sees_this_test_process() {
    let argv0 = std::env::args().next().unwrap();
    let base = argv0.rsplit('/').next().unwrap().to_string();
    assert!(find_pids(&base).contains(&(std::process::id() as i32)));
}

#[test]
fn unix_paths_map_to_wine_z_drive() {
    assert_eq!(
        to_wine_path(Path::new("/home/buga/.local/share/underwolf/a.dll")),
        r"Z:\home\buga\.local\share\underwolf\a.dll"
    );
}

#[test]
fn exe_matches_basename_case_insensitively() {
    assert!(exe_matches("Deadlock.EXE", "deadlock.exe"));
    assert!(exe_matches(r"S:\common\Deadlock\game\bin\win64\deadlock.exe", "deadlock.exe"));
    assert!(!exe_matches("deadlock.exe.bak", "deadlock.exe"));
}

#[test]
fn driver_loads_overlay_once_when_bootstrap_connects_fresh() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Fresh connection with no overlay: send load_overlay once.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: false }),
        vec![Command::LoadOverlay { path: r"Z:\games\overlay.dll".into() }]
    );
    // A later status does not re-send.
    assert_eq!(d.on_event(&Event::Status { code: "ok".into(), detail: 0, message: String::new() }), vec![]);
    assert!(d.is_done());
}

#[test]
fn driver_does_not_reload_after_client_restart() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Reconnect: the pinned bootstrap already has the overlay, so send nothing.
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: true }), vec![]);
    assert!(d.is_done());
}

#[test]
fn driver_with_no_overlay_sends_nothing() {
    let mut d = Driver::new(None);
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: false }), vec![]);
}
```

- [ ] **Step 3: Implement discovery (ported from the spike launcher)**

`crates/uw-inject/src/discovery.rs`:

```rust
//! Find a running Proton game and what it takes to run another program in its Wine session: the
//! game's `WINEPREFIX` and `WINE*SYNC` settings (from `/proc/<pid>/environ`), its wineserver
//! socket, and Proton's `files` directory (from the prefix's `config_info`). Ported and renamed
//! from the injection spike; no dependency on `spikes/`.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::exe_matches;

/// The Proton `files` directory (holding `bin/wine`) from a prefix's `config_info`, whose lines
/// name paths under it such as `.../Proton 11.0/files/lib/`.
pub fn proton_files_from_config_info(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let i = line.find("/files/")?;
        Some(PathBuf::from(&line[..i + "/files".len()]))
    })
}

/// Steam writes `config_info` into `compatdata/<appid>/`, next to the `pfx` prefix directory.
pub fn config_info_for_prefix(prefix: &Path) -> PathBuf {
    let compatdata = prefix.parent().unwrap_or(prefix);
    compatdata.join("config_info")
}

/// The game's Wine settings the helper must share to join its session: the prefix and the sync
/// mode (fsync/esync/ntsync must match the running wineserver).
pub fn wine_env(environ: &[u8]) -> Vec<(String, String)> {
    environ
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (key, value) = entry.split_once('=')?;
            let keep = key == "WINEPREFIX" || (key.starts_with("WINE") && key.ends_with("SYNC"));
            keep.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// Where a wineserver for the prefix with this device/inode listens (`server-<dev>-<ino>`).
pub fn server_socket(uid: u32, dev: u64, ino: u64) -> PathBuf {
    PathBuf::from(format!("/tmp/.wine-{uid}/server-{dev:x}-{ino:x}/socket"))
}

/// Pids whose argv0 names `exe`. Under Proton, a game's argv0 is its Windows path.
pub fn find_pids(exe: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut pids: Vec<i32> = dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|pid: &i32| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
                let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
                exe_matches(&String::from_utf8_lossy(argv0), exe)
            })
        })
        .collect();
    pids.sort_unstable();
    pids
}

/// The socket of the wineserver already running for `prefix`; an error if none is (Wine would
/// otherwise start a new, empty session the helper could not see the game in).
pub fn check_wine_session(prefix: &Path) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(prefix).map_err(|e| format!("prefix {}: {e}", prefix.display()))?;
    // SAFETY: getuid has no failure mode and no arguments.
    let uid = unsafe { libc::getuid() };
    let socket = server_socket(uid, meta.dev(), meta.ino());
    if socket.exists() {
        Ok(socket)
    } else {
        Err(format!("no running Wine session for {} (expected {})", prefix.display(), socket.display()))
    }
}

/// A typed discovery failure, so `uw-inject` can emit the design's `game_not_found` /
/// `no_wine_session` error codes instead of free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameError {
    /// The game exe is not running.
    NotRunning(String),
    /// The game runs but its Wine session could not be joined (no prefix, no wineserver, no
    /// Proton path, missing `wine`).
    NoWineSession(String),
}

impl GameError {
    /// The JSON-lines error code (design failure table, Linux side).
    pub fn code(&self) -> &'static str {
        match self {
            GameError::NotRunning(_) => "game_not_found",
            GameError::NoWineSession(_) => "no_wine_session",
        }
    }
}

impl std::fmt::Display for GameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameError::NotRunning(m) | GameError::NoWineSession(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for GameError {}

/// A running Proton game and its Wine session.
#[derive(Debug, Clone)]
pub struct GameSession {
    pub pid: i32,
    pub env: Vec<(String, String)>,
    pub prefix: PathBuf,
    /// Proton's `files` directory (holding `bin/wine`).
    pub proton: PathBuf,
}

impl GameSession {
    pub fn find(exe: &str, pid: Option<i32>) -> Result<GameSession, GameError> {
        GameSession::find_with_proton(exe, pid, None)
    }

    /// Like `find`, with an explicit Proton `files` directory instead of the one in `config_info`.
    pub fn find_with_proton(exe: &str, pid: Option<i32>, proton: Option<PathBuf>) -> Result<GameSession, GameError> {
        let pid = match pid {
            Some(p) => p,
            None => *find_pids(exe).first().ok_or_else(|| GameError::NotRunning(format!("{exe} is not running")))?,
        };
        let environ = std::fs::read(format!("/proc/{pid}/environ"))
            .map_err(|e| GameError::NoWineSession(format!("environ of {pid}: {e}")))?;
        let env = wine_env(&environ);
        let prefix = env
            .iter()
            .find(|(k, _)| k == "WINEPREFIX")
            .map(|(_, v)| PathBuf::from(v))
            .ok_or_else(|| GameError::NoWineSession(format!("pid {pid} has no WINEPREFIX (not a Wine process?)")))?;
        check_wine_session(&prefix).map_err(GameError::NoWineSession)?;
        let proton = match proton {
            Some(p) => p,
            None => {
                let info_path = config_info_for_prefix(&prefix);
                let info = std::fs::read_to_string(&info_path)
                    .map_err(|e| GameError::NoWineSession(format!("{}: {e}", info_path.display())))?;
                proton_files_from_config_info(&info)
                    .ok_or_else(|| GameError::NoWineSession(format!("no Proton path in {}", info_path.display())))?
            }
        };
        let wine = proton.join("bin/wine");
        if !wine.exists() {
            return Err(GameError::NoWineSession(format!("missing {}", wine.display())));
        }
        Ok(GameSession { pid, env, prefix, proton })
    }

    /// `wine <program> <args…>` in this game's Wine session, with our own shell's Wine variables
    /// removed so they cannot point the helper at another session.
    pub fn wine(&self, program: &Path, args: &[OsString]) -> Command {
        let mut cmd = Command::new(self.proton.join("bin/wine"));
        cmd.arg(program).args(args);
        for var in ["WINESERVER", "WINELOADER", "WINEDLLPATH", "WINEPREFIX"] {
            cmd.env_remove(var);
        }
        for (key, value) in &self.env {
            cmd.env(OsStr::new(key), value);
        }
        cmd
    }
}
```

- [ ] **Step 4: Implement the driver**

`crates/uw-inject/src/driver.rs`:

```rust
//! Turns the helper's JSON-lines events into the commands the host sends back. The only policy in
//! this plan: when the bootstrap connects fresh (no overlay yet) and an overlay path is
//! configured, send `load_overlay` once; after a client restart the bootstrap reports
//! `overlay_loaded: true`, so nothing is re-sent (design "Reconnect after a client restart").

use uw_pipe_proto::jsonl::{Command, Event};

pub struct Driver {
    overlay: Option<String>,
    sent_load: bool,
    connected: bool,
}

impl Driver {
    /// `overlay` is the Windows path to load, or `None` to connect only.
    pub fn new(overlay: Option<String>) -> Driver {
        Driver { overlay, sent_load: false, connected: false }
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded } => {
                self.connected = true;
                if *overlay_loaded {
                    // The pinned bootstrap already has the overlay (client restart): nothing to do.
                    self.sent_load = true;
                }
                match &self.overlay {
                    Some(path) if !overlay_loaded && !self.sent_load => {
                        self.sent_load = true;
                        vec![Command::LoadOverlay { path: path.clone() }]
                    }
                    _ => vec![],
                }
            }
            Event::Status { .. } | Event::Error { .. } => vec![],
        }
    }

    /// True once the bootstrap has connected and nothing more needs sending.
    pub fn is_done(&self) -> bool {
        self.connected && (self.overlay.is_none() || self.sent_load)
    }
}
```

- [ ] **Step 5: Run the tests (green)**

Run: `cargo test -p uw-inject`
Expected: `test result: ok. 12 passed` (the discovery test file; the binary has no tests yet).

- [ ] **Step 6: Commit**

```bash
git add crates/uw-inject
git commit -m "injection: uw-inject discovery, path conversion and JSON-lines driver"
```

---

## Task 4: `uw-helper` decision logic behind a `Win32` trait (unit-tested with a fake)

The helper's wait/hook/fallback/skip-on-marker/timeout/version decisions, portable and unit-tested on Linux with a fake, exactly as the collector's tasks are tested with `FakeMem`. Task 6 adds the real `Win32` implementation and the pipe wiring to the bin.

**Files:**
- Modify: `crates/uw-helper/Cargo.toml` (add `uw-pipe-proto` dependency, declare the lib module)
- Create: `crates/uw-helper/src/decide.rs`
- Modify: `crates/uw-helper/src/lib.rs` (add `pub mod decide;`)
- Test: `crates/uw-helper/tests/decide.rs`

- [ ] **Step 1: Add the dependency and module**

Edit `crates/uw-helper/Cargo.toml` to add a portable dependency (keep the existing `[lib]`, `[[bin]]` and `[target.'cfg(windows)'.dependencies]` sections from Task 1):

```toml
[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }
```

Append to `crates/uw-helper/src/lib.rs`:

```rust
pub mod decide;
```

- [ ] **Step 2: Write the decision tests first**

`crates/uw-helper/tests/decide.rs`:

```rust
use std::collections::VecDeque;

use uw_helper::decide::{hello_wait_from_bytes, run, Config, HelloWait, HookKind, Outcome, Win32};
use uw_pipe_proto::jsonl::Event;
use uw_pipe_proto::pipe::{encode, Message, StatusCode, PROTOCOL_VERSION};

struct Fake {
    now: u64,
    game: Option<u32>,
    ui: Option<u32>,
    marker: bool,
    lock_free: bool,
    pipe: Result<(), String>,
    getmessage_ok: bool,
    callwndproc_ok: bool,
    hellos: VecDeque<HelloWait>,
    log: Vec<String>,
}

impl Fake {
    fn happy() -> Fake {
        Fake {
            now: 0,
            game: Some(1000),
            ui: Some(2000),
            marker: false,
            lock_free: true,
            pipe: Ok(()),
            getmessage_ok: true,
            callwndproc_ok: true,
            hellos: VecDeque::new(),
            log: Vec::new(),
        }
    }
}

impl Win32 for Fake {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.now += ms;
    }
    fn find_game(&mut self) -> Option<u32> {
        self.log.push("find_game".into());
        self.game
    }
    fn find_ui_thread(&mut self, _pid: u32) -> Option<u32> {
        self.log.push("find_ui_thread".into());
        self.ui
    }
    fn marker_exists(&mut self, _pid: u32) -> bool {
        self.marker
    }
    fn take_helper_lock(&mut self, _pid: u32) -> bool {
        self.lock_free
    }
    fn create_pipe(&mut self, _pid: u32) -> Result<(), String> {
        self.log.push("create_pipe".into());
        self.pipe.clone()
    }
    fn set_hook(&mut self, kind: HookKind, _thread: u32) -> Result<(), u32> {
        self.log.push(format!("set_hook {kind:?}"));
        let ok = match kind {
            HookKind::GetMessage => self.getmessage_ok,
            HookKind::CallWndProc => self.callwndproc_ok,
        };
        if ok { Ok(()) } else { Err(1) }
    }
    fn post_wakeups(&mut self, _thread: u32) {
        self.log.push("post_wakeups".into());
    }
    fn wait_hello(&mut self, _timeout_ms: u64) -> HelloWait {
        self.log.push("wait_hello".into());
        self.hellos.pop_front().unwrap_or(HelloWait::Timeout)
    }
    fn unhook(&mut self) {
        self.log.push("unhook".into());
    }
}

fn cfg() -> Config {
    Config { window_timeout_ms: 20, hello_timeout_ms: 1000, window_poll_ms: 5, force_fallback: false }
}

fn hello(version: u16, overlay_loaded: bool, path: Option<&str>) -> HelloWait {
    HelloWait::Hello { protocol_version: version, overlay_loaded, overlay_path: path.map(Into::into) }
}

#[test]
fn happy_path_hooks_get_message_and_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    let out = run(&mut f, &cfg());
    assert_eq!(out, Outcome::Connected { overlay_loaded: false, overlay_path: None });
    assert!(f.log.contains(&"set_hook GetMessage".to_string()));
    assert!(!f.log.contains(&"set_hook CallWndProc".to_string()));
    assert_eq!(f.log.iter().filter(|c| *c == "unhook").count(), 1);
}

#[test]
fn second_helper_exits_already_running() {
    let mut f = Fake::happy();
    f.lock_free = false;
    assert_eq!(run(&mut f, &cfg()), Outcome::AlreadyRunning);
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
}

#[test]
fn no_game_within_the_window_timeout_is_no_window() {
    let mut f = Fake::happy();
    f.game = None;
    assert_eq!(run(&mut f, &cfg()), Outcome::NoWindow);
}

#[test]
fn game_without_a_window_is_no_window() {
    let mut f = Fake::happy();
    f.ui = None;
    assert_eq!(run(&mut f, &cfg()), Outcome::NoWindow);
}

#[test]
fn set_hook_failure_falls_back_to_callwndproc() {
    let mut f = Fake::happy();
    f.getmessage_ok = false;
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    let i = f.log.iter().position(|c| c == "set_hook GetMessage").unwrap();
    let j = f.log.iter().position(|c| c == "set_hook CallWndProc").unwrap();
    assert!(i < j, "GetMessage is tried before the CallWndProc fallback");
}

#[test]
fn both_hooks_failing_is_hook_failed() {
    let mut f = Fake::happy();
    f.getmessage_ok = false;
    f.callwndproc_ok = false;
    assert_eq!(run(&mut f, &cfg()), Outcome::HookFailed(1));
}

#[test]
fn timeout_then_fallback_hook_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(HelloWait::Timeout); // first hook: no hello
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None)); // fallback hook: hello
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    let i = f.log.iter().position(|c| c == "set_hook GetMessage").unwrap();
    let j = f.log.iter().position(|c| c == "set_hook CallWndProc").unwrap();
    assert!(i < j);
    assert_eq!(f.log.iter().filter(|c| *c == "unhook").count(), 2);
}

#[test]
fn timeout_on_both_hooks_is_bootstrap_timeout() {
    let mut f = Fake::happy();
    // Both wait_hello calls time out (queue empty -> Timeout).
    assert_eq!(run(&mut f, &cfg()), Outcome::BootstrapTimeout);
}

#[test]
fn mismatched_protocol_version_is_version_mismatch() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION + 1, false, None));
    assert_eq!(run(&mut f, &cfg()), Outcome::VersionMismatch);
}

#[test]
fn marker_present_skips_the_hook_and_takes_hello() {
    let mut f = Fake::happy();
    f.marker = true;
    f.hellos.push_back(hello(PROTOCOL_VERSION, true, Some(r"Z:\g\overlay.dll")));
    let out = run(&mut f, &cfg());
    assert_eq!(
        out,
        Outcome::Connected { overlay_loaded: true, overlay_path: Some(r"Z:\g\overlay.dll".into()) }
    );
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
    assert!(!f.log.iter().any(|c| c == "unhook"));
    assert!(f.log.contains(&"create_pipe".to_string()));
}

#[test]
fn force_fallback_sets_callwndproc_first() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    let mut c = cfg();
    c.force_fallback = true;
    assert_eq!(run(&mut f, &c), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    assert_eq!(f.log.iter().find(|c| c.starts_with("set_hook")).unwrap(), "set_hook CallWndProc");
}

#[test]
fn pipe_creation_failure_is_reported() {
    let mut f = Fake::happy();
    f.pipe = Err("busy".into());
    assert_eq!(run(&mut f, &cfg()), Outcome::PipeError("busy".into()));
}

/// A real hello frame, with the payload's protocol version and the header version set apart.
fn hello_bytes(protocol_version: u16, header_version: u16) -> Vec<u8> {
    let mut b = encode(&Message::Hello {
        protocol_version,
        bootstrap_version: 1,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
    b[6..8].copy_from_slice(&header_version.to_le_bytes()); // header `version` field
    b
}

#[test]
fn a_hello_from_another_protocol_version_is_version_mismatch() {
    // A newer bootstrap writes its own version into the header too; decode rejects the header,
    // and the helper must still report version_mismatch, not pipe_error.
    let mut f = Fake::happy();
    f.hellos.push_back(hello_wait_from_bytes(&hello_bytes(PROTOCOL_VERSION + 1, PROTOCOL_VERSION + 1)));
    assert_eq!(run(&mut f, &cfg()), Outcome::VersionMismatch);
}

#[test]
fn a_current_hello_frame_connects() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello_wait_from_bytes(&hello_bytes(PROTOCOL_VERSION, PROTOCOL_VERSION)));
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
}

#[test]
fn a_non_hello_first_message_is_a_pipe_error() {
    let b = encode(&Message::Status { request_id: 1, code: StatusCode::Ok, detail: 0, message: String::new() });
    assert!(matches!(hello_wait_from_bytes(&b), HelloWait::PipeError(_)));
}

#[test]
fn outcomes_map_to_json_events() {
    assert_eq!(
        Outcome::Connected { overlay_loaded: true, overlay_path: None }.to_event(),
        Event::BootstrapConnected { overlay_loaded: true }
    );
    assert_eq!(Outcome::NoWindow.to_event(), Event::error("no_window"));
    assert_eq!(Outcome::AlreadyRunning.to_event(), Event::error("already_running"));
    assert_eq!(Outcome::HookFailed(5).to_event(), Event::Error { code: "hook_failed".into(), detail: Some(5) });
    assert_eq!(Outcome::BootstrapTimeout.to_event(), Event::error("bootstrap_timeout"));
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
    assert_eq!(Outcome::PipeError("x".into()).to_event(), Event::error("pipe_error"));
}
```

- [ ] **Step 3: Implement the decision logic**

`crates/uw-helper/src/decide.rs`:

```rust
//! The helper's portable decision logic: wait for the game and its UI thread, take the helper
//! lock, skip the hook when the marker mutex already exists (the pinned bootstrap will reconnect),
//! otherwise set `WH_GETMESSAGE` (fall back to `WH_CALLWNDPROC` on a set failure or a hello
//! timeout), post wake-ups, and classify the `hello`. Side effects go through the `Win32` trait so
//! this is unit-tested on Linux with a fake; the real implementation lives in the `cfg(windows)`
//! bin.

use uw_pipe_proto::jsonl::Event;
use uw_pipe_proto::pipe::{decode, Message, ProtoError, PROTOCOL_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    GetMessage,
    CallWndProc,
}

fn other(kind: HookKind) -> HookKind {
    match kind {
        HookKind::GetMessage => HookKind::CallWndProc,
        HookKind::CallWndProc => HookKind::GetMessage,
    }
}

/// The first message read from the pipe after a hook is set (design step 6/7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloWait {
    Hello { protocol_version: u16, overlay_loaded: bool, overlay_path: Option<String> },
    Timeout,
    PipeError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    AlreadyRunning,
    NoWindow,
    HookFailed(u32),
    BootstrapTimeout,
    VersionMismatch,
    Connected { overlay_loaded: bool, overlay_path: Option<String> },
    PipeError(String),
}

impl Outcome {
    /// The JSON-lines event the helper emits for this outcome.
    pub fn to_event(&self) -> Event {
        match self {
            Outcome::Connected { overlay_loaded, .. } => {
                Event::BootstrapConnected { overlay_loaded: *overlay_loaded }
            }
            // `hook_failed` keeps its GetLastError as the event's `detail` (design failure table).
            Outcome::HookFailed(e) => Event::Error { code: "hook_failed".into(), detail: Some(*e) },
            Outcome::AlreadyRunning => Event::error("already_running"),
            Outcome::NoWindow => Event::error("no_window"),
            Outcome::BootstrapTimeout => Event::error("bootstrap_timeout"),
            Outcome::VersionMismatch => Event::error("version_mismatch"),
            // `pipe_error` is a helper-local I/O failure not in the design's failure table; it maps
            // here so the host still sees a structured error rather than a silent exit.
            Outcome::PipeError(_) => Event::error("pipe_error"),
        }
    }
}

pub struct Config {
    pub window_timeout_ms: u64,
    pub hello_timeout_ms: u64,
    pub window_poll_ms: u64,
    /// Force `WH_CALLWNDPROC` first (a test flag; design "forced `WH_CALLWNDPROC`").
    pub force_fallback: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            window_timeout_ms: 120_000,
            hello_timeout_ms: 10_000,
            window_poll_ms: 500,
            force_fallback: false,
        }
    }
}

/// Side effects the helper performs, abstracted so the logic above is unit-tested with a fake.
/// `<pid>` is the game's Windows (Wine) process id.
pub trait Win32 {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// The game's Windows pid, once Toolhelp finds it.
    fn find_game(&mut self) -> Option<u32>;
    /// The thread owning the game's largest visible top-level window without an owner.
    fn find_ui_thread(&mut self, pid: u32) -> Option<u32>;
    /// Whether `Local\underwolf-bootstrap-<pid>` already exists (a pinned bootstrap is present).
    fn marker_exists(&mut self, pid: u32) -> bool;
    /// Take `Local\underwolf-helper-<pid>`; `false` means another helper already holds it.
    fn take_helper_lock(&mut self, pid: u32) -> bool;
    /// Create the pipe server before hooking (design step 4).
    fn create_pipe(&mut self, pid: u32) -> Result<(), String>;
    /// Set a thread-scoped hook; `Err` carries `GetLastError`.
    fn set_hook(&mut self, kind: HookKind, thread: u32) -> Result<(), u32>;
    /// Post a few `WM_NULL` to the UI thread so its next message pump runs the hook.
    fn post_wakeups(&mut self, thread: u32);
    /// Accept the bootstrap and read its first message, or time out.
    fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait;
    /// Remove the current hook (the pin keeps the bootstrap loaded). A no-op if none is set.
    fn unhook(&mut self);
    /// Emit a diagnostic line (stderr in the real helper; a no-op in the fake).
    fn log(&self, _msg: &str) {}
}

/// Classify the first pipe message exactly as the helper reads it. A frame whose *header* version
/// differs (`BadVersion`) comes from a bootstrap of another protocol version, so it is reported as
/// a hello with that version: `run` then returns `VersionMismatch` (design failure table, "tells
/// the user to restart the game") instead of `PipeError`. Without this, `decode`'s header check
/// would make `version_mismatch` unreachable.
pub fn hello_wait_from_bytes(bytes: &[u8]) -> HelloWait {
    match decode(bytes) {
        Ok(Message::Hello { protocol_version, overlay_loaded, overlay_path, .. }) => HelloWait::Hello {
            protocol_version,
            overlay_loaded,
            overlay_path: (!overlay_path.is_empty()).then_some(overlay_path),
        },
        Err(ProtoError::BadVersion(v)) => {
            HelloWait::Hello { protocol_version: v, overlay_loaded: false, overlay_path: None }
        }
        Ok(_) => HelloWait::PipeError("expected hello".into()),
        Err(e) => HelloWait::PipeError(format!("decode hello: {e}")),
    }
}

fn outcome_from_hello(hw: HelloWait) -> Result<Outcome, ()> {
    match hw {
        HelloWait::Hello { protocol_version, overlay_loaded, overlay_path } => {
            if protocol_version != PROTOCOL_VERSION {
                Ok(Outcome::VersionMismatch)
            } else {
                Ok(Outcome::Connected { overlay_loaded, overlay_path })
            }
        }
        HelloWait::PipeError(e) => Ok(Outcome::PipeError(e)),
        HelloWait::Timeout => Err(()),
    }
}

/// Drive one helper run to an outcome.
pub fn run(w: &mut dyn Win32, cfg: &Config) -> Outcome {
    let start = w.now_ms();

    // 1. Find the game, waiting for it to appear within the window timeout.
    let pid = loop {
        if let Some(pid) = w.find_game() {
            break pid;
        }
        if w.now_ms().saturating_sub(start) >= cfg.window_timeout_ms {
            return Outcome::NoWindow;
        }
        w.sleep_ms(cfg.window_poll_ms);
    };

    // 2. One helper per game.
    if !w.take_helper_lock(pid) {
        return Outcome::AlreadyRunning;
    }

    // 3. Already there? The pinned bootstrap from an earlier helper will reconnect: create the
    //    pipe, skip the hook, and wait for its hello.
    if w.marker_exists(pid) {
        w.log("marker present; skipping hook");
        if let Err(e) = w.create_pipe(pid) {
            return Outcome::PipeError(e);
        }
        let hw = w.wait_hello(cfg.hello_timeout_ms);
        return outcome_from_hello(hw).unwrap_or(Outcome::BootstrapTimeout);
    }

    // 4. Hook. Find the UI thread (still within the window timeout).
    let ui = loop {
        if let Some(t) = w.find_ui_thread(pid) {
            break t;
        }
        if w.now_ms().saturating_sub(start) >= cfg.window_timeout_ms {
            return Outcome::NoWindow;
        }
        w.sleep_ms(cfg.window_poll_ms);
    };
    if let Err(e) = w.create_pipe(pid) {
        return Outcome::PipeError(e);
    }

    let first = if cfg.force_fallback { HookKind::CallWndProc } else { HookKind::GetMessage };

    // Set the first hook; a set failure falls back to the other hook immediately.
    let active = match w.set_hook(first, ui) {
        Ok(()) => first,
        Err(e) => match w.set_hook(other(first), ui) {
            Ok(()) => other(first),
            Err(_) => return Outcome::HookFailed(e),
        },
    };
    w.post_wakeups(ui);

    let hw = w.wait_hello(cfg.hello_timeout_ms);
    match outcome_from_hello(hw) {
        Ok(o) => {
            w.unhook();
            o
        }
        Err(()) => {
            // No hello: unhook and try the other hook once (design "try the fallback hook once").
            w.unhook();
            if w.set_hook(other(active), ui).is_err() {
                return Outcome::BootstrapTimeout;
            }
            w.post_wakeups(ui);
            let hw2 = w.wait_hello(cfg.hello_timeout_ms);
            let result = outcome_from_hello(hw2).unwrap_or(Outcome::BootstrapTimeout);
            w.unhook();
            result
        }
    }
}
```

- [ ] **Step 4: Run the decision tests (green)**

Run: `cargo test -p uw-helper --test decide`
Expected: `test result: ok. 16 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-helper
git commit -m "injection: helper decision logic behind a Win32 trait, with a fake"
```

---

## Task 5: Bootstrap worker (pipe connect, hello, overlay load, reconnect) and `uw-test-overlay`

Grow the bootstrap's worker from "write a proof file" into the real worker: create the marker mutex, connect to the pipe, send `hello`, serve `load_overlay`, and reconnect after the client restarts. Add the test overlay DLL. The overlay-load decision is portable and unit-tested on Linux (`state`); the FFI is proved under Wine in Tasks 6 and 8.

**Files:**
- Modify: `crates/uw-bootstrap/Cargo.toml` (add `uw-pipe-proto`; add windows-sys features), `crates/uw-bootstrap/src/lib.rs` (`pub mod state;`), `crates/uw-bootstrap/src/win.rs` (grow the worker)
- Create: `crates/uw-bootstrap/src/state.rs`, `crates/uw-bootstrap/tests/state.rs`
- Create: `crates/uw-test-overlay/Cargo.toml`, `crates/uw-test-overlay/src/lib.rs`

- [ ] **Step 1: Write the overlay-state tests first**

`crates/uw-bootstrap/tests/state.rs`:

```rust
// The crate's library name is `uw_bootstrap64` (the `[lib] name`), so the test imports that.
use uw_bootstrap64::state::{Decision, OverlayState};

#[test]
fn fresh_state_loads() {
    let s = OverlayState::new();
    assert_eq!(s.decide(r"Z:\g\overlay.dll"), Decision::Load);
    assert_eq!(s.loaded_path(), None);
}

#[test]
fn same_path_is_already_loaded_case_insensitively() {
    let mut s = OverlayState::new();
    s.mark_loaded(r"Z:\Games\Overlay.dll".into());
    assert_eq!(s.decide(r"z:\games\overlay.dll"), Decision::AlreadyLoaded);
    assert_eq!(s.loaded_path(), Some(r"Z:\Games\Overlay.dll"));
}

#[test]
fn a_different_path_is_already_loaded_other() {
    let mut s = OverlayState::new();
    s.mark_loaded(r"Z:\g\overlay.dll".into());
    assert_eq!(s.decide(r"Z:\g\other.dll"), Decision::AlreadyLoadedOther);
}
```

- [ ] **Step 2: Implement the state module and declare it**

Add to `crates/uw-bootstrap/src/lib.rs` (above the `#[cfg(windows)] mod win;` line):

```rust
pub mod state;
```

`crates/uw-bootstrap/src/state.rs`:

```rust
//! The bootstrap's overlay-load decision, portable so it is unit-tested on Linux. The bootstrap
//! loads at most one overlay and never unloads it (design "Out of scope"), so a repeated request
//! for the same path is `already_loaded` and a different path is refused (`already_loaded_other`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Load,
    AlreadyLoaded,
    AlreadyLoadedOther,
}

#[derive(Default)]
pub struct OverlayState {
    loaded: Option<String>,
}

impl OverlayState {
    pub fn new() -> OverlayState {
        OverlayState::default()
    }

    pub fn decide(&self, requested: &str) -> Decision {
        match &self.loaded {
            None => Decision::Load,
            Some(p) if p.eq_ignore_ascii_case(requested) => Decision::AlreadyLoaded,
            Some(_) => Decision::AlreadyLoadedOther,
        }
    }

    pub fn mark_loaded(&mut self, path: String) {
        self.loaded = Some(path);
    }

    pub fn loaded_path(&self) -> Option<&str> {
        self.loaded.as_deref()
    }
}
```

- [ ] **Step 3: Run the state tests (green)**

Run: `cargo test -p uw-bootstrap --test state`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 4: Add the dependency and windows-sys features**

Edit `crates/uw-bootstrap/Cargo.toml`: add the portable dependency and extend the windows-sys feature list:

```toml
[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Pipes",
    "Win32_System_SystemServices",
    "Win32_System_Threading",
    "Win32_UI_WindowsAndMessaging",
] }
```

- [ ] **Step 5: Grow the worker to connect the pipe and load the overlay**

Replace the **entire** file `crates/uw-bootstrap/src/win.rs` with the following. It keeps Task 1's hook procedure, self-pin and do-nothing `DllMain`, now beside the worker under one import block (so there are no duplicate imports):

```rust
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
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
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
pub extern "system" fn DllMain(_inst: HINSTANCE, reason: u32, _reserved: *mut core::ffi::c_void) -> i32 {
    let _ = (reason, DLL_PROCESS_ATTACH);
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
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !STARTED.swap(true, Ordering::AcqRel) {
            pin_self();
            let _ = std::thread::Builder::new().name("uw-bootstrap".into()).spawn(worker);
        }
    }));
    // SAFETY: a null hook handle forwards to the next hook (design: always).
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
/// pipe breaks.
fn serve_once(pid: u32, state: &mut OverlayState) {
    let pipe = loop {
        match connect(pid) {
            Some(h) => break h,
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
    if write_message(pipe, &hello).is_err() {
        close(pipe);
        return;
    }
    let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
    // `None` means the pipe broke: fall out and reconnect.
    while let Some(bytes) = read_message(pipe, &mut buf) {
        if !handle(pipe, &buf[..bytes], pid, state) {
            break; // a bad frame closes the connection (design: bootstrap reconnects)
        }
    }
    close(pipe);
}

/// Open the pipe client; `None` while the server pipe is missing or busy (both mean "retry").
fn connect(pid: u32) -> Option<HANDLE> {
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
    Some(h)
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

fn close(h: HANDLE) {
    // SAFETY: closes a handle we own.
    unsafe {
        CloseHandle(h);
    }
}
```

- [ ] **Step 6: Build the bootstrap for Windows (compile check)**

Run: `cargo build --release --target x86_64-pc-windows-gnu -p uw-bootstrap`
Expected: `Finished` with no errors; `target/x86_64-pc-windows-gnu/release/uw_bootstrap64.dll` exists.

- [ ] **Step 7: Create the test overlay**

`crates/uw-test-overlay/Cargo.toml`:

```toml
[package]
name = "uw-test-overlay"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` writes
# `%TEMP%\uw-test-overlay-<pid>.txt` and returns 0; the test reads it through the prefix's TEMP.
# Built for x86_64-pc-windows-gnu.
[lib]
name = "uw_test_overlay"
crate-type = ["cdylib"]

[target.'cfg(windows)'.dependencies]
# `CreateFileW` needs `Win32_Security`, `WriteFile` needs `Win32_System_IO` (see the bootstrap).
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_Threading",
] }
```

`crates/uw-test-overlay/src/lib.rs`:

```rust
//! A stand-in overlay for the Wine end-to-end tests: `uw_overlay_start` proves it ran by writing
//! `%TEMP%\uw-test-overlay-<pid>.txt` and returning 0. It ignores `StartParams` (it reads its own
//! pid), which is enough for the tests; the real overlay (plan 3) reads the parameters.

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetTempPathW, WriteFile, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    /// # Safety
    /// Called across the overlay FFI boundary; the pointer is ignored.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn uw_overlay_start(_params: *const core::ffi::c_void) -> u32 {
        // The overlay catches its own panics: a panic must not cross this `extern "system"`
        // boundary (it would abort; the bootstrap caller cannot catch it). 2 = panicked.
        std::panic::catch_unwind(run).unwrap_or(2)
    }

    fn run() -> u32 {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let mut buf = [0u16; 32768];
        // SAFETY: `buf` writable for its length.
        let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let mut path: Vec<u16> = buf[..n.min(buf.len())].to_vec();
        for c in format!("uw-test-overlay-{pid}.txt\0").encode_utf16() {
            path.push(c);
        }
        // SAFETY: NUL-terminated path; create and write a small file.
        unsafe {
            let h: HANDLE = CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return 1;
            }
            let msg = format!("overlay started in pid {pid}\n");
            let mut written = 0u32;
            let _ = WriteFile(h, msg.as_ptr(), msg.len() as u32, &mut written, std::ptr::null_mut());
            CloseHandle(h);
        }
        0
    }
}
```

- [ ] **Step 8: Build the test overlay (compile check)**

Run: `cargo build --release --target x86_64-pc-windows-gnu -p uw-test-overlay`
Expected: `Finished`; `target/x86_64-pc-windows-gnu/release/uw_test_overlay.dll` exists with a `uw_overlay_start` export (`x86_64-w64-mingw32-objdump -p … | grep uw_overlay_start`).

- [ ] **Step 9: Commit**

```bash
git add crates/uw-bootstrap crates/uw-test-overlay
git commit -m "injection: bootstrap pipe worker, overlay load, and test overlay"
```

---

## Task 6: Helper Win32 wiring (real `Win32`, pipe server, mutexes, JSON lines)

Replace Task 1's minimal helper bin with the full `cfg(windows)` implementation: the real `Win32` trait impl (window/thread lookup, hook, overlapped pipe server, mutexes), the JSON-lines loop on stdin/stdout driven by `decide::run`, and the post-connect relay (forward `load_overlay`, report `status`). Prove the happy path end-to-end under plain Wine (red → green).

**Files:**
- Modify: `crates/uw-helper/Cargo.toml` (windows-sys features for pipes/overlapped/mutex)
- Replace: `crates/uw-helper/src/bin/uw-helper64.rs`
- Modify: `crates/uw-helper/tests/wine_e2e.rs` (add the happy-path scenario)

- [ ] **Step 1: Extend the helper's windows-sys features**

Edit `crates/uw-helper/Cargo.toml`'s `[target.'cfg(windows)'.dependencies]` to the full set:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Gdi",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_Diagnostics_ToolHelp",
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Pipes",
    "Win32_System_SystemInformation",
    "Win32_System_Threading",
    "Win32_UI_WindowsAndMessaging",
] }
```

- [ ] **Step 2: Write the happy-path Wine test first**

Append to `crates/uw-helper/tests/wine_e2e.rs`. It reuses `setup`, `wine_cmd`, `to_wine_path`, `win_artifact` and `markers` from Task 1, and adds a `Helper` handle that Task 8 reuses. Two rules learned by running these tests under Wine:

- Read the helper's stdout on a **separate thread** and wait with `recv_timeout`. A blocking `lines.next()` cannot honour a deadline: if the helper is alive but silent, the test hangs forever instead of failing.
- End a helper by **killing** it (the design says "kill the helper, start a new one"). Closing its stdin also ends it (its relay loop sees EOF), but killing is unconditional and is what the reconnect scenario needs.

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::ChildStdin;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

/// A running `uw-helper64.exe`: its JSON-lines events arrive on a channel from a reader thread,
/// its stderr diagnostics accumulate in a shared buffer (so a test can assert which hook fired or
/// that the hook was skipped), commands go to its stdin. Dropping it kills the helper.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Helper {
    /// The next JSON-lines event, or a test failure after `within`.
    fn next_event(&self, within: Duration) -> String {
        self.events.recv_timeout(within).unwrap_or_else(|_| panic!("no helper event within {within:?}"))
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("helper stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Wait up to `within` for a stderr diagnostic line containing `needle`.
    fn wait_stderr(&self, needle: &str, within: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.stderr.lock().unwrap().iter().any(|l| l.contains(needle)) {
                return true;
            }
            if start.elapsed() >= within {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        wait_bounded(&mut self.child, Duration::from_secs(10));
    }
}

/// Start the helper against `uw-test-game.exe` in `pfx`, with extra args and environment.
fn start_helper(pfx: &Prefix, extra: &[&str], envs: &[(&str, &str)]) -> Helper {
    let mut cmd = wine_cmd(pfx, &win_artifact("uw-helper64.exe"));
    cmd.arg("uw-test-game.exe")
        .arg("--bootstrap")
        .arg(to_wine_path(&win_artifact("uw_bootstrap64.dll")))
        .args(extra)
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                break;
            }
        }
    });
    let diag = Arc::new(Mutex::new(Vec::new()));
    let diag2 = Arc::clone(&diag);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            diag2.lock().unwrap().push(line);
        }
    });
    Helper { child, stdin, events: rx, stderr: diag }
}

fn load_overlay_line(path_wine: &str) -> String {
    format!(r#"{{"cmd":"load_overlay","path":"{}"}}"#, path_wine.replace('\\', "\\\\"))
}

/// Drive the helper over JSON lines: connect, load the test overlay, expect `ok` and the marker.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn helper_connects_loads_overlay_and_reports_ok() {
    let mut pfx = setup("happy");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    // The first run boots the prefix, so allow up to 200 s for the connection.
    let connected = helper.next_event(Duration::from_secs(200));
    assert!(connected.contains(r#""event":"bootstrap_connected""#), "got: {connected}");

    helper.send(&load_overlay_line(&overlay));
    let status = helper.next_event(Duration::from_secs(30));
    assert!(status.contains(r#""event":"status""#) && status.contains(r#""code":"ok""#), "got: {status}");

    // The overlay wrote its marker file; find it by walking the prefix.
    assert_eq!(markers(&pfx, "uw-test-overlay-").len(), 1, "expected the overlay marker in the prefix");
}
```

Note: the full helper now serves the pipe instead of writing a proof file, so **delete the Task 1 proof test `hook_loads_bootstrap_into_the_game`** (its whole `#[test]` function and `#[ignore]` attribute) and the `WaitTimeout` trait, which only it used. Keep the module doc, the `use` lines, `wine`/`wineserver`/`win_artifact`/`to_wine_path`, `wait_bounded`, `Prefix`/`setup`/`wine_cmd` and `markers`. From here, this happy-path test is the hook mechanism's regression guard; Task 1's commit and the owner's Deadlock check hold the original proof.

- [ ] **Step 3: Replace the helper bin with the full implementation**

`crates/uw-helper/src/bin/uw-helper64.rs`:

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Finds the game's UI thread and sets a thread hook that
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`) on stdout, commands (`load_overlay`) on stdin, diagnostics on stderr.

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
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
        HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
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
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, GetCurrentProcessId, OpenMutexW, OpenProcess, ResetEvent,
        WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
        PostThreadMessageW, SendNotifyMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GW_OWNER,
        HHOOK, HOOKPROC, WH_CALLWNDPROC, WH_GETMESSAGE, WM_NULL,
    };
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};

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

    struct Found {
        pid: u32,
        best_area: i64,
        thread: u32,
        hwnd: HWND,
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        // No Rust panic may cross this FFI boundary: catch and keep enumerating.
        let keep = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `lparam` points at the caller's `Found`.
            let f = unsafe { &mut *(lparam as *mut Found) };
            let mut pid = 0u32;
            // SAFETY: `hwnd` from EnumWindows; writes one u32.
            let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            // SAFETY: querying an enumerated top-level window.
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
        let _ = keep;
        1
    }

    struct RealWin32 {
        exe: String,
        bootstrap: String,
        pid: u32,
        ui_thread: u32,
        hwnd: HWND,
        pipe: HANDLE,
        hook: HHOOK,
        module: HMODULE,
        helper_mutex: HANDLE,
        event: HANDLE,
        next_request: u32,
    }

    impl RealWin32 {
        fn new(exe: String, bootstrap: String) -> RealWin32 {
            RealWin32 {
                exe,
                bootstrap,
                pid: 0,
                ui_thread: 0,
                hwnd: std::ptr::null_mut(),
                pipe: INVALID_HANDLE_VALUE,
                hook: std::ptr::null_mut(),
                module: std::ptr::null_mut(),
                helper_mutex: std::ptr::null_mut(),
                event: std::ptr::null_mut(),
                next_request: 1,
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
            // SAFETY: transmute to the hook procedure type.
            Some(Some(unsafe {
                std::mem::transmute::<_, unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>(p)
            }))
        }

        /// Wake the UI thread so the next message pump runs the hook. Both a posted message
        /// (`WH_GETMESSAGE`) and a sent message (`WH_CALLWNDPROC`, which only fires on sent ones).
        fn rewake(&self) {
            // SAFETY: both post/send WM_NULL to the UI thread / its main window.
            unsafe {
                PostThreadMessageW(self.ui_thread, WM_NULL, 0, 0);
                if !self.hwnd.is_null() {
                    SendNotifyMessageW(self.hwnd, WM_NULL, 0, 0);
                }
            }
        }

        /// Wait on the overlapped event up to `timeout_ms`, re-waking every 500 ms (so an idle
        /// `WH_CALLWNDPROC` hook still fires). `true` once the operation completes.
        fn wait_rewake(&self, ov: *mut OVERLAPPED, timeout_ms: u64) -> bool {
            let mut left = timeout_ms;
            loop {
                let slice = left.min(500);
                // SAFETY: waits on our event; then collects the overlapped result.
                let r = unsafe { WaitForSingleObject(self.event, slice as u32) };
                if r == WAIT_OBJECT_0 {
                    let mut n = 0u32;
                    return unsafe { GetOverlappedResult(self.pipe, ov, &mut n, 0) } != 0;
                }
                left = left.saturating_sub(slice);
                if left == 0 {
                    return false;
                }
                self.rewake();
            }
        }

        /// Cancel a pending overlapped op and wait for the cancel to finish, so its `OVERLAPPED`
        /// is safe to drop.
        fn cancel(&self, ov: *mut OVERLAPPED) {
            // SAFETY: cancels I/O on our pipe and blocks until the cancel completes.
            unsafe {
                CancelIoEx(self.pipe, ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, ov, &mut n, 1);
            }
        }

        /// Overlapped `ConnectNamedPipe` + read of the first message, bounded by `timeout_ms`,
        /// re-waking the UI thread throughout.
        fn connect_and_read_hello(&mut self, timeout_ms: u64) -> HelloWait {
            // SAFETY: start an overlapped connect on our event.
            unsafe {
                ResetEvent(self.event);
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = self.event;
                let ok = ConnectNamedPipe(self.pipe, &mut ov);
                if ok == 0 {
                    let e = GetLastError();
                    if e != ERROR_IO_PENDING && e != ERROR_PIPE_CONNECTED {
                        return HelloWait::PipeError(format!("ConnectNamedPipe: {e}"));
                    }
                    if e == ERROR_IO_PENDING && !self.wait_rewake(&mut ov, timeout_ms) {
                        self.cancel(&mut ov);
                        DisconnectNamedPipe(self.pipe);
                        return HelloWait::Timeout;
                    }
                }
            }
            match self.read_message(timeout_ms) {
                // Portable and unit-tested; maps another header version to `version_mismatch`.
                Some(bytes) => hello_wait_from_bytes(&bytes),
                None => HelloWait::Timeout,
            }
        }

        /// Read one message (message mode, overlapped) within `timeout_ms`.
        fn read_message(&self, timeout_ms: u64) -> Option<Vec<u8>> {
            let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
            // SAFETY: overlapped read into `buf` bounded by the event wait.
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
                    if WaitForSingleObject(self.event, timeout_ms as u32) != WAIT_OBJECT_0 {
                        self.cancel(&mut ov);
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
            // SAFETY: overlapped write; wait for completion, cancelling on timeout.
            unsafe {
                ResetEvent(self.event);
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = self.event;
                let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
                if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                    return false;
                }
                if WaitForSingleObject(self.event, 15_000) != WAIT_OBJECT_0 {
                    self.cancel(&mut ov);
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
                    Ok(Command::LoadOverlay { path }) => {
                        if path.encode_utf16().count() > MAX_PATH_UNITS {
                            emit(&Event::error("path_too_long"));
                            continue;
                        }
                        let request_id = self.next_request;
                        self.next_request += 1;
                        if !self.write_message(&Message::LoadOverlay { request_id, path }) {
                            emit(&Event::error("game_exited"));
                            return;
                        }
                        match self.read_message(15_000).and_then(|b| decode(&b).ok()) {
                            Some(Message::Status { code, detail, message, .. }) => {
                                emit(&Event::Status { code: code.as_str().into(), detail, message });
                            }
                            _ => {
                                emit(&Event::error("status_timeout"));
                            }
                        }
                    }
                    Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
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
            emit(&Event::error("game_exited"));
            std::process::exit(0);
        });
    }

    impl Win32 for RealWin32 {
        fn now_ms(&self) -> u64 {
            // SAFETY: a monotonic tick count is fine for the window/hello timeouts.
            unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
        }
        fn sleep_ms(&mut self, ms: u64) {
            // SAFETY: a plain sleep.
            unsafe { windows_sys::Win32::System::Threading::Sleep(ms as u32) };
        }
        fn find_game(&mut self) -> Option<u32> {
            // SAFETY: a process snapshot walked with a correctly sized PROCESSENTRY32W.
            unsafe {
                let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                let mut e: PROCESSENTRY32W = std::mem::zeroed();
                e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                let mut ok = Process32FirstW(snap, &mut e) != 0;
                while ok {
                    if e.th32ProcessID != GetCurrentProcessId() && basename_eq(&e.szExeFile, &self.exe) {
                        self.pid = e.th32ProcessID;
                        CloseHandle(snap);
                        return Some(e.th32ProcessID);
                    }
                    ok = Process32NextW(snap, &mut e) != 0;
                }
                CloseHandle(snap);
            }
            None
        }
        fn find_ui_thread(&mut self, pid: u32) -> Option<u32> {
            let mut f = Found { pid, best_area: 0, thread: 0, hwnd: std::ptr::null_mut() };
            // SAFETY: the callback only writes through `&mut f`.
            unsafe { EnumWindows(Some(enum_cb), &mut f as *mut Found as LPARAM) };
            if f.thread != 0 {
                self.ui_thread = f.thread;
                self.hwnd = f.hwnd;
                Some(f.thread)
            } else {
                None
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
            // SAFETY: no arguments.
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
            // SAFETY: a manual-reset auto event for overlapped I/O.
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
        fn post_wakeups(&mut self, thread: u32) {
            for _ in 0..5 {
                // SAFETY: posts WM_NULL to the UI thread.
                unsafe { PostThreadMessageW(thread, WM_NULL, 0, 0) };
            }
            self.rewake();
        }
        fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait {
            self.connect_and_read_hello(timeout_ms)
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
        if let Outcome::Connected { .. } = outcome {
            w.relay();
            0
        } else {
            eprintln!("[uw-helper64] {outcome:?}");
            1
        }
    }
}
```

- [ ] **Step 4: Confirm the helper builds for Windows**

Run: `cargo build --release --target x86_64-pc-windows-gnu -p uw-helper`
Expected: `Finished`; `target/x86_64-pc-windows-gnu/release/uw-helper64.exe` exists.

- [ ] **Step 5: Run the happy-path Wine test (green)**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
xvfb-run -a cargo test -p uw-helper --test wine_e2e helper_connects_loads_overlay_and_reports_ok -- --ignored --test-threads=1
```
Expected: `helper_connects_loads_overlay_and_reports_ok ... ok`, `test result: ok. 1 passed`. The helper's stdout carries `{"event":"bootstrap_connected","overlay_loaded":false}` then `{"event":"status","code":"ok","detail":0,"message":"overlay started"}`, and one `uw-test-overlay-<pid>.txt` appears in the prefix user's `AppData/Local/Temp`.

- [ ] **Step 6: Commit**

```bash
git add crates/uw-helper
git commit -m "injection: full helper Win32 wiring, pipe server, and JSON-lines relay"
```

---

## Task 7: `uw-inject` CLI (Linux)

The operator entry point: find the game and its Wine session, start `uw-helper64.exe` inside it, and drive it with the `Driver` over JSON lines, printing events. The command construction is unit-tested on Linux (like the spike's `wine_command` test); the full chain is proved under Wine in Task 8 and in Deadlock in Task 9.

**Files:**
- Modify: `crates/uw-inject/src/discovery.rs` (add `helper_command`)
- Create: `crates/uw-inject/src/bin/uw-inject.rs`
- Test: `crates/uw-inject/tests/cli.rs`

- [ ] **Step 1: Write the command-construction test first**

`crates/uw-inject/tests/cli.rs`:

```rust
#![cfg(unix)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use uw_inject::discovery::{helper_command, GameSession};

fn session() -> GameSession {
    GameSession {
        pid: 42,
        env: vec![
            ("WINEPREFIX".to_string(), "/s/compatdata/1/pfx".to_string()),
            ("WINEFSYNC".to_string(), "1".to_string()),
        ],
        prefix: PathBuf::from("/s/compatdata/1/pfx"),
        proton: PathBuf::from("/p/Proton 11.0/files"),
    }
}

#[test]
fn helper_command_runs_helper_in_the_game_session() {
    let cmd = helper_command(
        &session(),
        Path::new("/home/u/.local/share/underwolf/uw-helper64.exe"),
        "deadlock.exe",
        Path::new("/home/u/.local/share/underwolf/uw_bootstrap64.dll"),
        false,
    );
    assert_eq!(cmd.get_program(), OsStr::new("/p/Proton 11.0/files/bin/wine"));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(
        args,
        [
            OsStr::new("/home/u/.local/share/underwolf/uw-helper64.exe"),
            OsStr::new("deadlock.exe"),
            OsStr::new("--bootstrap"),
            OsStr::new(r"Z:\home\u\.local\share\underwolf\uw_bootstrap64.dll"),
        ]
    );
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("WINEPREFIX"), Some(OsStr::new("/s/compatdata/1/pfx")))));
    assert!(envs.contains(&(OsStr::new("WINEFSYNC"), Some(OsStr::new("1")))));
    // Our own shell's Wine settings must not leak in and point at another session.
    assert!(envs.contains(&(OsStr::new("WINESERVER"), None)));
}

#[test]
fn helper_command_passes_fallback_flag() {
    let cmd = helper_command(
        &session(),
        Path::new("/x/uw-helper64.exe"),
        "deadlock.exe",
        Path::new("/x/uw_bootstrap64.dll"),
        true,
    );
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(args.last(), Some(&OsStr::new("--fallback")));
}
```

- [ ] **Step 2: Add `helper_command` to discovery**

Append to `crates/uw-inject/src/discovery.rs`:

```rust
use crate::to_wine_path;

/// `wine <helper> <game-exe> --bootstrap <Z:…bootstrap> [--fallback]` in the game's Wine session.
pub fn helper_command(
    session: &GameSession,
    helper: &Path,
    game_exe: &str,
    bootstrap: &Path,
    force_fallback: bool,
) -> Command {
    let mut args = vec![
        OsString::from(game_exe),
        OsString::from("--bootstrap"),
        OsString::from(to_wine_path(bootstrap)),
    ];
    if force_fallback {
        args.push(OsString::from("--fallback"));
    }
    session.wine(helper, &args)
}
```

- [ ] **Step 3: Run the CLI test (green)**

Run: `cargo test -p uw-inject --test cli`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 4: Write the `uw-inject` binary**

`crates/uw-inject/src/bin/uw-inject.rs`:

```rust
//! Linux launcher: find a running Proton game and drive the Wine-side helper.
//!
//!   uw-inject [--pid N] [--exe deadlock.exe] [--proton DIR] [--helper PATH]
//!             [--bootstrap PATH] [--overlay PATH] [--fallback]
//!
//! Finds the game and its Wine session, starts `uw-helper64.exe` inside it, prints the helper's
//! JSON-lines events, and — when an overlay is given — sends `load_overlay` once the bootstrap
//! connects fresh (the `Driver`). Defaults: `deadlock.exe`; Proton from the prefix's `config_info`;
//! helper and bootstrap from `~/.local/share/underwolf/`. Ctrl-C (SIGINT) or SIGTERM ends the
//! helper too; the pinned bootstrap stays in the game for the next run.

#[cfg(not(unix))]
fn main() {
    eprintln!("uw-inject is the Linux launcher");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("[uw-inject] {msg}");
            std::process::exit(1);
        }
    }
}

/// The running helper's pid, for the signal handler (0 = none yet).
#[cfg(unix)]
static HELPER_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Ending the client ends the helper (design "Reconnect after a client restart"): on SIGINT or
/// SIGTERM, kill the helper, then exit. The pinned bootstrap stays in the game. Only
/// async-signal-safe calls (an atomic load, `kill`, `_exit`) happen here.
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    let pid = HELPER_PID.load(std::sync::atomic::Ordering::SeqCst);
    // SAFETY: `kill` and `_exit` are async-signal-safe.
    unsafe {
        if pid > 0 {
            libc::kill(pid, libc::SIGKILL);
        }
        libc::_exit(130);
    }
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::Stdio;

    use uw_inject::discovery::{helper_command, GameSession};
    use uw_inject::driver::Driver;
    use uw_inject::to_wine_path;
    use uw_pipe_proto::jsonl::Event;

    let mut pid: Option<i32> = None;
    let mut exe = "deadlock.exe".to_string();
    let mut proton: Option<PathBuf> = None;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let dir = home.join(".local/share/underwolf");
    let mut helper = dir.join("uw-helper64.exe");
    let mut bootstrap = dir.join("uw_bootstrap64.dll");
    let mut overlay: Option<PathBuf> = None;
    let mut fallback = false;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--pid" => pid = Some(value()?.parse().map_err(|e| format!("--pid: {e}"))?),
            "--exe" => exe = value()?,
            "--proton" => proton = Some(value()?.into()),
            "--helper" => helper = value()?.into(),
            "--bootstrap" => bootstrap = value()?.into(),
            "--overlay" => overlay = Some(value()?.into()),
            "--fallback" => fallback = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let session = match GameSession::find_with_proton(&exe, pid, proton) {
        Ok(s) => s,
        Err(e) => {
            // Report the design's Linux-side code on stdout; the detail goes to stderr.
            println!("{}", Event::error(e.code()).to_line());
            eprintln!("[uw-inject] {e}");
            return Ok(1);
        }
    };
    for p in [&helper, &bootstrap] {
        if !p.exists() {
            return Err(format!("missing {}", p.display()));
        }
    }
    if let Some(o) = &overlay {
        if !o.exists() {
            return Err(format!("missing {}", o.display()));
        }
    }
    eprintln!("[uw-inject] {exe} pid {}; prefix {}", session.pid, session.prefix.display());

    let mut cmd = helper_command(&session, &helper, &exe, &bootstrap, fallback);
    if std::env::var_os("WINEDEBUG").is_none() {
        cmd.env("WINEDEBUG", "-all");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running the helper: {e}"))?;
    HELPER_PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs a handler that only makes async-signal-safe calls.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut driver = Driver::new(overlay.as_deref().map(to_wine_path));

    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("reading the helper: {e}"))?;
        let Ok(event) = Event::parse(&line) else {
            continue;
        };
        println!("{line}");
        if matches!(&event, Event::Error { code, .. } if code == "version_mismatch") {
            eprintln!("[uw-inject] restart the game to load the new bootstrap");
        }
        for cmd in driver.on_event(&event) {
            writeln!(stdin, "{}", cmd.to_line()).map_err(|e| format!("writing to the helper: {e}"))?;
            stdin.flush().ok();
        }
    }
    let status = child.wait().map_err(|e| format!("waiting for the helper: {e}"))?;
    Ok(status.code().unwrap_or(1))
}
```

- [ ] **Step 5: Confirm the CLI builds (Linux)**

Run: `cargo build -p uw-inject --bin uw-inject`
Expected: `Finished`; `target/debug/uw-inject` exists.

- [ ] **Step 6: Commit**

```bash
git add crates/uw-inject
git commit -m "injection: uw-inject CLI drives the helper over JSON lines"
```

---

## Task 8: The Wine end-to-end test suite

Every design "Testing" item #2 scenario, under plain Wine (ignored by default). Helper-level scenarios extend `crates/uw-helper/tests/wine_e2e.rs`; the full-chain `uw-inject` scenario is `crates/uw-inject/tests/wine_e2e.rs`.

**Files:**
- Modify: `crates/uw-helper/tests/wine_e2e.rs` (add the remaining scenarios)
- Create: `crates/uw-inject/tests/wine_e2e.rs`

- [ ] **Step 1: Add the helper-level scenarios**

Append to `crates/uw-helper/tests/wine_e2e.rs`. These reuse `setup`, `wine_cmd`, `to_wine_path`, `win_artifact`, `markers`, `Helper`, `start_helper` and `load_overlay_line` (Task 6). Each `Helper` is killed when it goes out of scope.

```rust
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn reloading_the_same_path_is_already_loaded_and_another_is_already_loaded_other() {
    let mut pfx = setup("reload");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let other = to_wine_path(&win_artifact("uw_bootstrap64.dll")); // a different, existing DLL

    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));

    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"already_loaded""#));
    helper.send(&load_overlay_line(&other));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"already_loaded_other""#));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_new_helper_reuses_the_pinned_bootstrap_without_rehooking() {
    let mut pfx = setup("reconnect");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    // Helper 1: connect and load the overlay. Leaving the block kills it (design: "kill the
    // helper, start a new one"); its pipe and helper mutex go with it.
    {
        let mut first = start_helper(&pfx, &[], &[]);
        assert!(first.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
        first.send(&load_overlay_line(&overlay));
        assert!(first.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
    }

    // Helper 2: the marker mutex exists, so it skips the hook, and the pinned bootstrap reconnects
    // with overlay_loaded: true (design "Reconnect after a client restart").
    let second = start_helper(&pfx, &[], &[]);
    let connected = second.next_event(Duration::from_secs(60));
    assert!(connected.contains("bootstrap_connected"), "got: {connected}");
    assert!(connected.contains(r#""overlay_loaded":true"#), "got: {connected}");
    // It reused the pinned bootstrap without re-hooking (design: "no new hook").
    assert!(second.wait_stderr("marker present; skipping hook", Duration::from_secs(5)));

    // The overlay was not loaded a second time: still exactly one marker file.
    assert_eq!(markers(&pfx, "uw-test-overlay-").len(), 1);
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn forced_callwndproc_fallback_also_connects_and_loads() {
    let mut pfx = setup("fallback");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &["--fallback"], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    // The connection must have come through WH_CALLWNDPROC, not a WH_GETMESSAGE fallback after a
    // timeout (design "forced WH_CALLWNDPROC"; the wake-ups make it fire — I1).
    assert!(helper.wait_stderr("hooked WH_CALLWNDPROC", Duration::from_secs(5)));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_game_without_a_window_reports_no_window() {
    let mut pfx = setup("nowindow");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--no-window").spawn().unwrap(),
    );
    // A short window timeout instead of the 120 s default; the prefix boot still counts against
    // the overall event deadline below.
    let helper = start_helper(&pfx, &[], &[("UW_WINDOW_TIMEOUT_MS", "8000")]);
    let event = helper.next_event(Duration::from_secs(200));
    assert!(event.contains(r#""event":"error""#) && event.contains(r#""code":"no_window""#), "got: {event}");
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn an_overlay_without_the_export_reports_no_start_export() {
    let mut pfx = setup("noexport");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    // The bootstrap DLL exists and loads but has no `uw_overlay_start` export.
    let no_export = to_wine_path(&win_artifact("uw_bootstrap64.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&no_export));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"no_start_export""#));
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn killing_the_game_reports_game_exited() {
    let mut pfx = setup("gameexit");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));

    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(200)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&overlay));
    assert!(helper.next_event(Duration::from_secs(30)).contains(r#""code":"ok""#));

    // Kill the game while the helper is idle on stdin; the watcher thread must notice and report
    // `game_exited` (design failure table), without waiting for the next stdin command.
    if let Some(mut g) = pfx.game.take() {
        let _ = g.kill();
        wait_bounded(&mut g, Duration::from_secs(10));
    }
    let event = helper.next_event(Duration::from_secs(20));
    assert!(event.contains(r#""event":"error""#) && event.contains(r#""code":"game_exited""#), "got: {event}");
}
```

- [ ] **Step 2: Add the full-chain `uw-inject` test**

`crates/uw-inject/tests/wine_e2e.rs`:

```rust
#![cfg(unix)]
//! The whole Linux chain under plain Wine: `uw-inject` finds the test game in a scratch prefix,
//! starts `uw-helper64.exe` in that Wine session, the bootstrap connects, and the test overlay is
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. Ignored by
//! default (needs Wine and the Windows artifacts).
//!
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap \
//!       -p uw-test-overlay -p uw-helper
//!   xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use uw_inject::discovery::find_pids;

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}
fn win_artifact(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release")
        .join(name);
    assert!(p.exists(), "missing {} (build the Windows artifacts first)", p.display());
    p
}

struct Fixture {
    prefix: PathBuf,
    game: Child,
    /// The game's Linux pid in THIS prefix, passed to `uw-inject --pid` so it never picks a stray
    /// `uw-test-game.exe` from another prefix (the review's leftover-process hazard).
    game_pid: i32,
}
/// Wait up to `d` for `child` to exit, then kill it (bounded teardown, as in the helper tests).
fn wait_bounded(child: &mut Child, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.game.kill();
        wait_bounded(&mut self.game, Duration::from_secs(10));
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.prefix).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.prefix.parent().unwrap());
    }
}

/// The game's Linux pid running in `prefix`: a `uw-test-game.exe` pid whose `/proc/<pid>/environ`
/// has this prefix's `WINEPREFIX`. `None` until Wine has booted the game.
fn game_pid_in(prefix: &Path) -> Option<i32> {
    let want = format!("WINEPREFIX={}", prefix.display());
    find_pids("uw-test-game.exe").into_iter().find(|pid| {
        std::fs::read(format!("/proc/{pid}/environ"))
            .map(|e| e.split(|&b| b == 0).any(|v| v == want.as_bytes()))
            .unwrap_or(false)
    })
}

/// Start the test game in a fresh prefix and wait until it shows up in `/proc` for this prefix
/// (the first run boots the prefix, which takes a while).
fn start_game(tag: &str) -> Fixture {
    let prefix = std::env::temp_dir()
        .join(format!("uw-inject-chain-{tag}-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&prefix).unwrap();
    let game = Command::new(wine())
        .arg(win_artifact("uw-test-game.exe"))
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .spawn()
        .unwrap();
    let start = Instant::now();
    let game_pid = loop {
        if let Some(p) = game_pid_in(&prefix) {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(180), "uw-test-game.exe never started");
        std::thread::sleep(Duration::from_millis(500));
    };
    Fixture { prefix, game, game_pid }
}

/// Files starting with `stem` in the prefix users' `%TEMP%`
/// (`drive_c/users/<user>/AppData/Local/Temp`). **No recursion:** `dosdevices/z: -> /` means a
/// recursive walk would follow that symlink out into the whole filesystem and never finish.
fn markers(prefix: &Path, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for user in std::fs::read_dir(prefix.join("drive_c/users")).into_iter().flatten().flatten() {
        let temp = user.path().join("AppData/Local/Temp");
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(stem) {
                out.push(e.path());
            }
        }
    }
    out
}

/// Run `uw-inject --proton <wine-root> --exe uw-test-game.exe …` (it finds the running game by
/// exe name), collect its printed events until one contains `until` (or `within` passes), then
/// end it with SIGTERM — which, by design, also ends its helper. Events are read on a thread so
/// the deadline always holds.
fn run_inject(fx: &Fixture, until: &str, within: Duration) -> Vec<String> {
    // `--proton <root>` is any Wine install with `bin/wine`; the system Wine's root is /usr.
    let proton_root = Path::new(&wine()).parent().unwrap().parent().unwrap().to_path_buf();
    let mut child = Command::new(env!("CARGO_BIN_EXE_uw-inject"))
        .args(["--exe", "uw-test-game.exe", "--pid"])
        .arg(fx.game_pid.to_string())
        .arg("--proton")
        .arg(&proton_root)
        .arg("--helper")
        .arg(win_artifact("uw-helper64.exe"))
        .arg("--bootstrap")
        .arg(win_artifact("uw_bootstrap64.dll"))
        .arg("--overlay")
        .arg(win_artifact("uw_test_overlay.dll"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + within;
    let mut events = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(event) => {
                let done = event.contains(until);
                events.push(event);
                if done {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    // SAFETY: signals our own child process.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    wait_bounded(&mut child, Duration::from_secs(10));
    events
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn uw_inject_connects_and_loads_the_overlay() {
    let fx = start_game("load");
    let events = run_inject(&fx, r#""code":"ok""#, Duration::from_secs(200));
    assert!(events.iter().any(|e| e.contains("bootstrap_connected")), "events: {events:?}");
    assert!(events.iter().any(|e| e.contains(r#""code":"ok""#)), "events: {events:?}");
    assert_eq!(markers(&fx.prefix, "uw-test-overlay-").len(), 1, "the overlay marker should exist in the prefix");

    // A second uw-inject run (client restart) reconnects without reloading the overlay.
    let events2 = run_inject(&fx, "bootstrap_connected", Duration::from_secs(60));
    assert!(
        events2.iter().any(|e| e.contains("bootstrap_connected") && e.contains(r#""overlay_loaded":true"#)),
        "events2: {events2:?}"
    );
    assert_eq!(markers(&fx.prefix, "uw-test-overlay-").len(), 1, "the overlay must not load twice");
}
```

- [ ] **Step 3: Build everything and run the whole Wine suite**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
```
Expected: the helper suite reports `7 passed` (the happy path from Task 6 plus the six scenarios here), the inject suite `1 passed`. Each test cleans up its scratch prefix and kills its wineserver on drop. Run the scenarios one at a time (`cargo test -p uw-helper --test wine_e2e <name> -- --ignored`) when debugging; each boots its own prefix, so a full run takes several minutes.

**Status when this plan was written:** the happy path (hook → `hello` → `load_overlay` → `ok` → marker in the prefix) and the reconnect sequence (kill helper 1, helper 2 gets `overlay_loaded:true` in ~1 s) were both run by hand under Wine 11.20 with these artifacts and passed. An earlier version of this harness hung at the final `markers()` assert — not in teardown — because the old recursive `markers` followed the prefix's `dosdevices/z: -> /` symlink and walked the whole filesystem forever; the non-recursive `markers` above fixes that, and every teardown wait is bounded (`wait_bounded`). Run the suite under `xvfb-run -a` with `--test-threads=1` so no game window reaches the screen and only one prefix boots at a time. If `a_new_helper_reuses_the_pinned_bootstrap_without_rehooking` fails, check that killing helper 1 ends its `uw-helper64.exe` and that helper 2 can retake the pipe (`FILE_FLAG_FIRST_PIPE_INSTANCE`) and the helper mutex; fix the cause in the helper or bootstrap, not by loosening the test.

- [ ] **Step 4: Commit**

```bash
git add crates/uw-helper crates/uw-inject
git commit -m "injection: Wine end-to-end test suite for every design scenario"
```

---

## Task 9: Owner live check in Deadlock (Proton)

The design's "Testing" item #3. The owner runs this against a live Deadlock under Proton 11.0. The implementer prepares the commands and the install; the owner runs them and reports back.

**Files:** none (verification + a short doc note of the result).

- [ ] **Step 1: Build and install the artifacts**

Run (implementer, then hand the owner the commands below):
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-bootstrap -p uw-helper -p uw-test-overlay
cargo build --release -p uw-inject --bin uw-inject
dir="$HOME/.local/share/underwolf"
mkdir -p "$dir"
win="target/x86_64-pc-windows-gnu/release"
# Copy then rename, so a game that already loaded a DLL keeps its copy intact.
for src in "$win/uw_bootstrap64.dll" "$win/uw-helper64.exe" "$win/uw_test_overlay.dll" "target/release/uw-inject"; do
    cp "$src" "$dir/$(basename "$src").tmp" && mv -f "$dir/$(basename "$src").tmp" "$dir/$(basename "$src")"
done
```

- [ ] **Step 2: Owner live check — connect and load the overlay (Deadlock running)**

Owner runs (Deadlock running under Proton 11.0):
```bash
# Step 2 (Deadlock running under Proton 11.0):
grep ClientVersion ~/.local/share/Steam/steamapps/common/Deadlock/game/citadel/steam.inf
#   expect one line, e.g. ClientVersion=6813 (record it with the result)
GAME=$(pgrep -x deadlock.exe); echo "$GAME"            # expect one pid
~/.local/share/underwolf/uw-inject --overlay ~/.local/share/underwolf/uw_test_overlay.dll
#   expect stdout, in order:
#     {"event":"bootstrap_connected","overlay_loaded":false}
#     {"event":"status","code":"ok","detail":0,"message":"overlay started"}
#   and stderr: [uw-helper64] hooked WH_GETMESSAGE thread=…   (record which hook)
#   then press Ctrl-C (ends uw-inject and its helper; the bootstrap stays pinned)
ls ~/.local/share/Steam/steamapps/compatdata/1422450/pfx/drive_c/users/steamuser/AppData/Local/Temp/uw-test-overlay-*.txt
#   expect exactly 1 file
pgrep -x deadlock.exe                                  # expect the same pid: the game still runs
```
**Deadlock keeps running normally.** If the first event is `{"event":"error","code":"no_window"}` or `bootstrap_timeout`, retry with `--fallback` appended (the stderr line then reads `hooked WH_CALLWNDPROC`). **If it still fails, STOP and report to the owner.**

- [ ] **Step 3: Owner live check — restart reconnects without reloading**

Run this only after Step 2's `uw-inject` was stopped with Ctrl-C. While a first `uw-inject` still runs, its helper holds `Local\underwolf-helper-<pid>` and the pipe, so a second run would print `{"event":"error","code":"already_running"}`.
```bash
# Step 3 (after Step 2's uw-inject was stopped with Ctrl-C):
~/.local/share/underwolf/uw-inject --overlay ~/.local/share/underwolf/uw_test_overlay.dll
#   expect stdout: {"event":"bootstrap_connected","overlay_loaded":true}
#   and stderr: [uw-helper64] marker present; skipping hook
#   then Ctrl-C
ls ~/.local/share/Steam/steamapps/compatdata/1422450/pfx/drive_c/users/steamuser/AppData/Local/Temp/uw-test-overlay-*.txt | wc -l
#   expect 1 (the overlay was not loaded again)
pgrep -x deadlock.exe                                  # expect the same pid
```
**Deadlock keeps running normally** throughout.

- [ ] **Step 4: Record the result**

Note the outcome (pass/fail, the `ClientVersion`, which hook fired from the Step 2 stderr line, any observations) at the bottom of
`docs/superpowers/specs/2026-10-22-injection-design.md` under a new "Live check (owner)" line, or in the plan report. Commit only if a file changed:

```bash
git commit -am "injection: record the Deadlock live-check result"
```

---

## Task 10: Done-check (no remote-thread injection; all tests green)

The design's "Done when". A pure verification task; nothing to implement.

**Files:** none.

- [ ] **Step 1: No remote-thread injection anywhere under `crates/`**

Run:
```bash
! grep -rnE 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx' crates/
```
Expected: the command prints nothing and succeeds (exit 0). Any match is a plan failure — the design forbids these three calls under `crates/`.

- [ ] **Step 2: All Linux unit tests pass**

Run:
```bash
cargo test -p uw-pipe-proto -p uw-inject -p uw-helper -p uw-bootstrap
```
Expected: every crate's tests pass (`uw-pipe-proto`: names (1) + `pipe` (10) + `jsonl` (5); `uw-inject`: discovery/driver (12) + `cli` (2); `uw-helper`: `decide` (16); `uw-bootstrap`: `state` (3)). No failures.

- [ ] **Step 3: All Windows artifacts build**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu \
    -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
```
Expected: `Finished`, and `target/x86_64-pc-windows-gnu/release/` holds `uw-test-game.exe`, `uw_bootstrap64.dll`, `uw_test_overlay.dll`, `uw-helper64.exe`.

- [ ] **Step 4: All Wine end-to-end tests pass**

Run:
```bash
cargo build -p uw-inject --bin uw-inject
xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
```
Expected: `uw-helper` Wine suite `7 passed`, `uw-inject` Wine suite `1 passed`.

- [ ] **Step 5: Confirm the owner live check passed**

Confirm Task 9's live check passed (recorded result). With Steps 1–5 all green, this plan is done.

There is no code change in this task, so there is nothing to commit.

---

## Self-review (completed by the plan author)

**1. Spec coverage** — every design requirement maps to a task:

- Find the game + Wine session (design flow 1): Task 3 (`discovery`), Task 7 (CLI).
- Find the UI thread; largest visible top-level window without owner (flow 2): Task 1 (minimal), Task 6 (`find_ui_thread`).
- Already there? marker mutex → skip hook (flow 3): Task 4 (logic), Task 6 (`marker_exists`), Task 8 (reconnect e2e).
- Hook + `WH_CALLWNDPROC` fallback, helper mutex, pipe-first, `LoadLibraryW` into helper, wake-ups (flow 4): Task 1 (minimal hook), Task 4 (logic), Task 6 (real).
- Bootstrap start: first-call pin + one worker, else `CallNextHookEx` (flow 5): Task 1.
- Connect, marker mutex, hello, retry (flow 6): Task 5.
- Unhook on hello, `bootstrap_connected` (flow 7): Task 4/Task 6.
- Load overlay, path→`Z:`, `uw_overlay_start`, `status`, `already_loaded` (flow 8): Task 3 (path), Task 5 (bootstrap), Task 6 (relay), Task 8.
- Pipe binary format, header, limits, string rules, status codes (Messages): Task 2.
- Overlay entry point, `StartParams`, panic catch, `DllMain` nothing (Messages): Task 5 (`StartParams`, `catch_unwind`, no-op `DllMain`), Task 1 (`DllMain`).
- JSON lines Linux↔helper (Messages): Task 2 (`jsonl`), Task 6 (emit/relay), Task 7 (drive).
- Failure table (every row): `game_not_found`/`no_wine_session` Task 3; `no_window`/`hook_failed`/`bootstrap_timeout`/`version_mismatch`/`already_running` Task 4 + Task 6 + Task 8; overlay status codes Task 5; `already_loaded_other` Task 5/Task 8; `game_exited` Task 6 relay.
- Reconnect after client restart: Task 4 (logic), Task 5 (bootstrap reconnect), Task 8 + Task 9.
- Panics never cross FFI; `panic = "unwind"`: Task 5 (`catch_unwind`); the "Why these facts" note records that release unwinds by default.
- Platform facts (hook needs DLL-resident proc; pin; loader lock; retryable `CreateFileW` errors): Task 1/Task 6 (load bootstrap into helper), Task 1 (`PIN`), Task 5 (load on worker thread; `ERROR_FILE_NOT_FOUND`/`ERROR_PIPE_BUSY` retry).
- Code layout: six crates exactly as the design's table; workspace `members` gains `crates/*`; nothing depends on `spikes/`. 32-bit left room for via the `64` names.
- Testing #1 (unit), #2 (Wine e2e), #3 (owner live check): Tasks 2–5 (unit), Task 6 + Task 8 (Wine), Task 9 (Deadlock).
- Done when: Task 10.
- Task 1 is the gate (prove the hook, else stop): Task 1 Steps 6–7.

**2. Placeholder scan** — no `TODO`/`TBD`/"add error handling"/"similar to Task N"; every code step shows complete code; every run step gives an exact command and expected output.

**3. Type consistency** — names match across tasks: `Message`/`StatusCode`/`decode`/`encode` (`uw-pipe-proto::pipe`); `Command`/`Event` (`jsonl`); `pipe_name`/`helper_mutex_name`/`marker_mutex_name` (`names`); `Win32`/`HookKind`/`HelloWait`/`Outcome`/`Config`/`run` (`uw-helper::decide`); `OverlayState`/`Decision` (`uw-bootstrap::state`); `GameSession`/`helper_command`/`find_pids` (`uw-inject::discovery`); `Driver` (`uw-inject::driver`); exports `uw_bootstrap_hook`, `uw_overlay_start`; artifacts `uw_bootstrap64.dll`, `uw-helper64.exe`, `uw_test_overlay.dll`, `uw-test-game.exe`.

**Additions beyond the design (all test/operability aids, not new features):**
- `pipe_error` and `status_timeout` / `path_too_long` / `game_exited` are helper-local JSON error
  codes. `game_exited` is in the design's failure table; the others are structured reports for
  conditions the design leaves as free behaviour, so the host never sees a silent exit.
- `UW_WINDOW_TIMEOUT_MS` / `UW_HELLO_TIMEOUT_MS` let the Wine tests shorten the 120 s / 10 s
  defaults; unset in production, the design's timeouts stand.
- `uw-inject --fallback` and `uw-helper64 --fallback` force `WH_CALLWNDPROC` so the design's
  fallback hook can be exercised on demand (Task 8) and retried in the gate/live check.

**Items from the design with no task:** none. Everything in the design is scheduled.
