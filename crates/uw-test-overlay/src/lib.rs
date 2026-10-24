//! A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` proves it ran by writing
//! `%TEMP%\uw-test-overlay-<pid>.txt`, then starts the control channel's worker
//! (`uw_game_channel::start`, module version 1) and the test producer, and returns 0. On every
//! bell it writes `%TEMP%\uw-test-state-<pid>.txt` (`seq=<n> edit_mode=<0|1> visible=<0|1>`) and
//! then acknowledges that `seq` (`applied`), which rings `state_applied`. It ignores
//! `StartParams` (it reads its own pid), which is enough for the tests; the real overlay (plan 3)
//! reads them.
//!
//! The test producer (design 2b "Testing" 3) runs on a thread of its own: data channel 1, category
//! `test` with keys `ticks` and `last_seq` (32 bytes each, `last_seq` null at first). Every second
//! it pushes a `tick` event (data: the tick count) and commits `ticks`; after each acknowledged
//! state it pushes a `state_seen` event (data: the seq) and commits `last_seq`. It flushes every
//! 100 ms.
//!
//! In the game's environment, `UW_TEST_OVERLAY_START_DELAY_MS` makes `uw_overlay_start` take that
//! long (the late-reply test), and `UW_TEST_OVERLAY_BELL_DELAY_MS` makes every bell take that long
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).
//! `UW_TEST_OVERLAY_DATA_RESTART_MS` drops the producer after that long and creates it again once
//! the helper has let go of the old objects (the disconnect test). `UW_TEST_OVERLAY_BURST=<n>`
//! pushes n `burst` events (data: the count from 0, zero-padded to 1000 bytes) before each
//! `state_seen` (the burst test).

#[cfg(windows)]
mod win {
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    use uw_game_channel::data::Producer;
    use uw_pipe_proto::data::{Schema, SchemaCategory};
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetTempPathW,
        WriteFile,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    const MODULE_VERSION: u32 = 1;
    const DATA_CHANNEL: u32 = 1;
    const FLUSH: Duration = Duration::from_millis(100);
    const TICK: Duration = Duration::from_secs(1);

    /// # Safety
    /// Called across the overlay FFI boundary; the pointer is ignored.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn uw_overlay_start(_params: *const core::ffi::c_void) -> u32 {
        // The overlay catches its own panics: a panic must not cross this `extern "system"`
        // boundary (it would abort; the bootstrap caller cannot catch it). 2 = panicked.
        std::panic::catch_unwind(run).unwrap_or(2)
    }

    /// A delay in ms from the environment variable `name`, if set.
    fn env_delay(name: &str) -> Option<Duration> {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).map(Duration::from_millis)
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
        let (seen_tx, seen_rx) = channel();
        let restart_after = env_delay("UW_TEST_OVERLAY_DATA_RESTART_MS");
        let burst = std::env::var("UW_TEST_OVERLAY_BURST").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if std::thread::Builder::new()
            .name("uw-test-producer".into())
            .spawn(move || produce(&seen_rx, restart_after, burst))
            .is_err()
        {
            return 4;
        }
        let started = uw_game_channel::start(MODULE_VERSION, move |link, _bits, state| {
            if let Some(d) = bell_delay {
                std::thread::sleep(d);
            }
            let text = format!("seq={} edit_mode={} visible={}\n", state.seq, state.edit_mode as u8, state.visible as u8);
            write_temp(&format!("uw-test-state-{pid}.txt"), &text);
            match link.applied(state.seq) {
                Ok(()) => {
                    let _ = seen_tx.send(state.seq);
                }
                Err(e) => eprintln!("[uw-test-overlay] applied: {e:?}"),
            }
        });
        if started.is_err() { 3 } else { 0 }
    }

    /// Create the test producer, retrying every 100 ms while the old objects still exist (a
    /// helper holds them until it sees `disconnect`), and commit its first values. It runs on the
    /// producer's own thread, never on a game thread.
    fn create_producer(ticks: u64, last_seq: Option<u64>) -> Producer {
        let schema = Schema::new(vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])])
            .expect("the test schema fits");
        let mut last_error = String::new();
        let mut p = loop {
            match Producer::create(DATA_CHANNEL, schema.clone()) {
                Ok(p) => break p,
                Err(e) => {
                    // Logged once per distinct error, as the helper logs its open errors.
                    let e = e.to_string();
                    if e != last_error {
                        eprintln!("[uw-test-overlay] data channel: {e}; retrying");
                        last_error = e;
                    }
                    std::thread::sleep(FLUSH);
                }
            }
        };
        let (ticks, last_seq) = (ticks.to_string(), last_seq.map(|s| s.to_string()));
        if let Err(e) = p.commit(&[("test", "ticks", Some(&ticks)), ("test", "last_seq", last_seq.as_deref())]) {
            eprintln!("[uw-test-overlay] commit: {e}");
        }
        p
    }

    /// The test producer's thread: ticks every second, `state_seen` after each applied state
    /// (after `burst` events, if any), a flush every 100 ms.
    fn produce(seen: &Receiver<u64>, restart_after: Option<Duration>, burst: usize) {
        let mut ticks = 0u64;
        let mut last_seq = None;
        let mut p = create_producer(ticks, last_seq);
        let mut restart_at = restart_after.map(|d| Instant::now() + d);
        let mut next_tick = Instant::now() + TICK;
        loop {
            match seen.recv_timeout(FLUSH) {
                Ok(seq) => {
                    for i in 0..burst {
                        if let Err(e) = p.push_event("burst", &format!("{i:0>1000}")) {
                            eprintln!("[uw-test-overlay] push_event: {e}");
                        }
                    }
                    last_seq = Some(seq);
                    let seq = seq.to_string();
                    if let Err(e) = p.push_event("state_seen", &seq) {
                        eprintln!("[uw-test-overlay] push_event: {e}");
                    }
                    if let Err(e) = p.commit(&[("test", "last_seq", Some(&seq))]) {
                        eprintln!("[uw-test-overlay] commit: {e}");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // The worker's callback is gone: nothing more to report, but keep ticking.
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(FLUSH),
            }
            if Instant::now() >= next_tick {
                next_tick += TICK;
                ticks += 1;
                let n = ticks.to_string();
                if let Err(e) = p.push_event("tick", &n) {
                    eprintln!("[uw-test-overlay] push_event: {e}");
                }
                if let Err(e) = p.commit(&[("test", "ticks", Some(&n))]) {
                    eprintln!("[uw-test-overlay] commit: {e}");
                }
            }
            if restart_at.is_some_and(|t| Instant::now() >= t) {
                restart_at = None;
                drop(p);
                p = create_producer(ticks, last_seq);
            }
            p.flush();
        }
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
