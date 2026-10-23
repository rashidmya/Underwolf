//! A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` proves it ran by writing
//! `%TEMP%\uw-test-overlay-<pid>.txt`, then starts the control channel's worker
//! (`uw_game_channel::start`, module version 1) and returns 0. On every bell it writes
//! `%TEMP%\uw-test-state-<pid>.txt` (`seq=<n> edit_mode=<0|1> visible=<0|1>`) and then
//! acknowledges that `seq` (`applied`), which rings `state_applied`. It ignores `StartParams` (it
//! reads its own pid), which is enough for the tests; the real overlay (plan 3) reads them.
//!
//! In the game's environment, `UW_TEST_OVERLAY_START_DELAY_MS` makes `uw_overlay_start` take that
//! long (the late-reply test), and `UW_TEST_OVERLAY_BELL_DELAY_MS` makes every bell take that long
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetTempPathW, WriteFile, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    const MODULE_VERSION: u32 = 1;

    /// # Safety
    /// Called across the overlay FFI boundary; the pointer is ignored.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn uw_overlay_start(_params: *const core::ffi::c_void) -> u32 {
        // The overlay catches its own panics: a panic must not cross this `extern "system"`
        // boundary (it would abort; the bootstrap caller cannot catch it). 2 = panicked.
        std::panic::catch_unwind(run).unwrap_or(2)
    }

    /// A delay in ms from the environment variable `name`, if set.
    fn env_delay(name: &str) -> Option<std::time::Duration> {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).map(std::time::Duration::from_millis)
    }

    fn run() -> u32 {
        if let Some(d) = env_delay("UW_TEST_OVERLAY_START_DELAY_MS") {
            std::thread::sleep(d);
        }
        let bell_delay = env_delay("UW_TEST_OVERLAY_BELL_DELAY_MS");
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        if !write_temp(&format!("uw-test-overlay-{pid}.txt"), &format!("overlay started in pid {pid}\n")) {
            return 1;
        }
        let started = uw_game_channel::start(MODULE_VERSION, move |link, _bits, state| {
            if let Some(d) = bell_delay {
                std::thread::sleep(d);
            }
            let text = format!("seq={} edit_mode={} visible={}\n", state.seq, state.edit_mode as u8, state.visible as u8);
            write_temp(&format!("uw-test-state-{pid}.txt"), &text);
            if let Err(e) = link.applied(state.seq) {
                eprintln!("[uw-test-overlay] applied: {e:?}");
            }
        });
        if started.is_err() { 3 } else { 0 }
    }

    /// Write `text` to `%TEMP%\<name>` (replacing it). `false` if it could not be created.
    fn write_temp(name: &str, text: &str) -> bool {
        let mut buf = [0u16; 32768];
        // SAFETY: `buf` writable for its length.
        let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let mut path: Vec<u16> = buf[..n.min(buf.len())].to_vec();
        path.extend(name.encode_utf16().chain(Some(0)));
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
                return false;
            }
            let mut written = 0u32;
            let _ = WriteFile(h, text.as_ptr(), text.len() as u32, &mut written, std::ptr::null_mut());
            CloseHandle(h);
        }
        true
    }
}
