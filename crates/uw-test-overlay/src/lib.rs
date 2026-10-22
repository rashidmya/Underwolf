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
