//! Data gate probe, producer side (design 2b "Testing" 2). Run under Wine next to
//! `data_consumer_probe`, in the same prefix. It creates channel 1 of its own process
//! (`Producer::create`) with the schema `probe` {`a`: 16, `b`: 16}, commits `a = "1"` and
//! `b = null`, pushes three events (`first`/`a`, `second`/`b`, `third`/`c`), flushes, and prints
//! `ready pid=<windows pid> producer_id=<the producer's own id>`. Then it runs one command per stdin line and answers each with one
//! line, `ok` unless said otherwise:
//! - `delta`: commit `a = "2"`, push `fourth`/`d`, flush;
//! - `burst <n>`: push n events `burst` (data `0`, `1`, …), then flush until all are written (the
//!   throttle writes at most 30 per 200 ms);
//! - `hold-info <ms>`: a thread takes the info lock and holds it for ms (`ok` once it has it);
//! - `abandon-events`: a thread takes the events lock and ends without releasing it;
//! - `abandon-info`: a thread takes the info lock, breaks the info store's magic, and ends
//!   without releasing it;
//! - `drop`: drop the producer (it sets `disconnect`);
//! - `recreate`: `Producer::create` again: `ok`, or `exists` while a consumer still holds the
//!   old objects.
//!
//! It exits when stdin closes.

#[cfg(not(windows))]
fn main() {
    eprintln!("data_producer_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::{BufRead, Write};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use uw_game_channel::data::{CreateError, Producer};
    use uw_pipe_proto::data::{Schema, SchemaCategory};
    use uw_pipe_proto::names::{DataObject, data_name};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Memory::{
        FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, INFINITE, MUTEX_MODIFY_STATE, OpenMutexW, ReleaseMutex,
        SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
    };

    const CHANNEL: u32 = 1;

    fn say(s: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    }

    fn schema() -> Schema {
        Schema::new(vec![SchemaCategory::new("probe", &[("a", 16), ("b", 16)])])
            .expect("the probe schema fits")
    }

    fn name(object: DataObject) -> Vec<u16> {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        data_name(pid, CHANNEL, object)
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    /// Open one of our channel's locks by name, apart from the producer's own handle.
    fn open_lock(object: DataObject) -> HANDLE {
        let n = name(object);
        // SAFETY: opens an existing named mutex to wait on it and release it.
        let h = unsafe {
            OpenMutexW(
                SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                0,
                n.as_ptr(),
            )
        };
        assert!(!h.is_null(), "OpenMutexW failed");
        h
    }

    /// On a thread of its own: take `object`'s lock, run `then` while holding it, and either
    /// release it after `hold_ms` or end the thread without releasing it (`hold_ms = None`), which
    /// abandons it. Answers `ok` once the lock is held.
    fn lock_on_thread(object: DataObject, hold_ms: Option<u64>, then: fn()) {
        let (tx, rx) = channel();
        let t = std::thread::spawn(move || {
            let h = open_lock(object);
            // SAFETY: waits on the mutex handle opened just above.
            let r = unsafe { WaitForSingleObject(h, INFINITE) };
            if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
                // Dropping `tx` unsent makes the caller answer the failure.
                return;
            }
            then();
            let _ = tx.send(());
            if let Some(ms) = hold_ms {
                std::thread::sleep(Duration::from_millis(ms));
                // SAFETY: releases the mutex this thread acquired just above.
                unsafe { ReleaseMutex(h) };
            }
            // SAFETY: closes the handle opened above. A mutex still owned is abandoned when this
            // thread ends.
            unsafe { CloseHandle(h) };
        });
        if rx.recv().is_err() {
            say("error lock thread failed");
            return;
        }
        if hold_ms.is_none() {
            let _ = t.join();
        }
        say("ok");
    }

    fn break_info_magic() {
        let n = name(DataObject::Info);
        // SAFETY: opens our own info mapping for writing, writes one byte of its magic (both
        // handles checked first), and unmaps and closes it again.
        unsafe {
            let h = OpenFileMappingW(FILE_MAP_WRITE, 0, n.as_ptr());
            assert!(!h.is_null(), "OpenFileMappingW failed");
            let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 0);
            assert!(!view.Value.is_null(), "MapViewOfFile failed");
            *view.Value.cast::<u8>() = b'X';
            UnmapViewOfFile(view);
            CloseHandle(h);
        }
    }

    fn create() -> Result<Producer, CreateError> {
        let mut p = Producer::create(CHANNEL, schema())?;
        p.commit(&[("probe", "a", Some("1")), ("probe", "b", None)])
            .expect("commit");
        for (name, data) in [("first", "a"), ("second", "b"), ("third", "c")] {
            p.push_event(name, data).expect("push_event");
        }
        p.flush();
        Ok(p)
    }

    pub fn main() -> i32 {
        let mut producer = match create() {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[data_producer_probe] create: {e}");
                return 3;
            }
        };
        let producer_id = producer.as_ref().map_or(0, Producer::producer_id);
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        say(&format!("ready pid={pid} producer_id={producer_id}"));
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let mut words = line.split_whitespace();
            match (words.next().unwrap_or(""), producer.as_mut()) {
                ("delta", Some(p)) => {
                    p.commit(&[("probe", "a", Some("2"))]).expect("commit");
                    p.push_event("fourth", "d").expect("push_event");
                    p.flush();
                    say("ok");
                }
                ("burst", Some(p)) => {
                    let n: u64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                    for i in 0..n {
                        p.push_event("burst", &i.to_string()).expect("push_event");
                    }
                    while p.queued() > 0 {
                        p.flush();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    say("ok");
                }
                ("hold-info", Some(_)) => {
                    let ms = words.next().and_then(|w| w.parse().ok()).unwrap_or(1000);
                    lock_on_thread(DataObject::InfoLock, Some(ms), || {});
                }
                ("abandon-events", Some(_)) => lock_on_thread(DataObject::EventsLock, None, || {}),
                ("abandon-info", Some(_)) => {
                    lock_on_thread(DataObject::InfoLock, None, break_info_magic)
                }
                ("drop", Some(_)) => {
                    producer = None;
                    say("ok");
                }
                ("recreate", None) => match create() {
                    Ok(p) => {
                        producer = Some(p);
                        say("ok");
                    }
                    Err(CreateError::Exists(_)) => say("exists"),
                    Err(e) => say(&format!("error {e}")),
                },
                (other, _) => say(&format!("error unknown command {other:?}")),
            }
        }
        0
    }
}
