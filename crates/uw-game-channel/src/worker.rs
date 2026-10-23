//! The in-game worker (design 2a "Components and flow"). One thread, started by the overlay
//! module, for the whole game session:
//! 1. connect to the overlay pipe (retry once a second while it is missing or busy);
//! 2. send `overlay_hello` (module version, state layout);
//! 3. open the doorbells and the state block (`Objects::open`). The previous connection's objects
//!    are dropped only after the new ones are open, so a block the game holds survives a helper
//!    restart (design "Failures");
//! 4. wait on the pipe and `bell-game`: answer `ping` with `pong`, and on a bell swap `to_game`,
//!    read the block under the lock and hand the bits and the values to the overlay's callback;
//! 5. on a lost connection, start again at 1.
//!
//! It is the only thread in the game that waits on events or on the state lock (design
//! "Threads"). A panic, in the callback too, ends only the current connection.

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

use uw_pipe_proto::bells::to_host;
use uw_pipe_proto::names::overlay_pipe_name;
use uw_pipe_proto::pipe::{decode_overlay, encode_overlay, OverlayMessage, HEADER_LEN, MAX_PAYLOAD};
use uw_pipe_proto::state::{self, StateV1, LAYOUT_VERSION};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{SetNamedPipeHandleState, PIPE_READMODE_MESSAGE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, ResetEvent, Sleep, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::objects::{LockError, Objects, LOCK_TIMEOUT_MS};

/// Clients retry once a second without limit (design "Differences from Overwolf").
const RETRY_MS: u32 = 1000;
/// How long one pipe write may take.
const WRITE_TIMEOUT_MS: u32 = 5000;

static STARTED: AtomicBool = AtomicBool::new(false);

/// What the overlay's callback gets: acknowledge an applied state with `applied`.
pub struct Link<'a> {
    objects: &'a Objects,
    seen: &'a Cell<u64>,
}

impl Link<'_> {
    /// The overlay applied the state with this `seq`: write `applied_seq` under the state lock
    /// (design: 100 ms, then `state_busy`), then ring `state_applied`. `ping` answers with the
    /// last `seq` given here.
    pub fn applied(&self, seq: u64) -> Result<(), LockError> {
        let mut g = self.objects.lock(LOCK_TIMEOUT_MS)?;
        state::set_applied_seq(g.bytes_mut(), seq);
        drop(g);
        self.seen.set(seq);
        self.objects.ring_host(to_host::STATE_APPLIED);
        Ok(())
    }
}

/// Start the worker (once per process). `module_version` goes into `overlay_hello`.
/// `on_bells(link, bits, state)` runs on the worker thread for every bell from the host: the
/// `to_game` bits (unknown ones included; ignore them) and the block as read right after.
pub fn start<F>(module_version: u32, on_bells: F) -> std::io::Result<()>
where
    F: FnMut(&Link<'_>, u64, &StateV1) + Send + 'static,
{
    if STARTED.swap(true, Ordering::AcqRel) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "the channel worker already runs"));
    }
    std::thread::Builder::new()
        .name("uw-game-channel".into())
        .spawn(move || worker(module_version, on_bells))
        .map(|_| ())
}

fn worker<F: FnMut(&Link<'_>, u64, &StateV1)>(module_version: u32, mut on_bells: F) {
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    // Kept across connections: see step 3 in the module docs. `seen` is the last `seq` the overlay
    // applied (what `pong` reports); the block it belongs to survives a reconnect too.
    let mut objects: Option<Objects> = None;
    let seen = Cell::new(0u64);
    loop {
        let served =
            catch_unwind(AssertUnwindSafe(|| serve_once(pid, module_version, &mut objects, &seen, &mut on_bells)));
        if served.is_err() {
            eprintln!("[uw-game-channel] serve panicked; reconnecting");
        }
        // SAFETY: a plain sleep.
        unsafe { Sleep(RETRY_MS) };
    }
}

fn serve_once<F: FnMut(&Link<'_>, u64, &StateV1)>(
    pid: u32,
    module_version: u32,
    objects: &mut Option<Objects>,
    seen: &Cell<u64>,
    on_bells: &mut F,
) {
    let mut client = loop {
        match Client::connect(&overlay_pipe_name(pid)) {
            Some(c) => break c,
            // SAFETY: a plain sleep before retrying.
            None => unsafe { Sleep(RETRY_MS) },
        }
    };
    if !client.write(&encode_overlay(&OverlayMessage::Hello { module_version, state_layout: LAYOUT_VERSION })) {
        return;
    }
    match Objects::open(pid) {
        // The old objects drop here, after the new ones are open.
        Ok(o) => *objects = Some(o),
        Err(e) => {
            eprintln!("[uw-game-channel] open: {e}");
            return;
        }
    }
    let o = objects.as_ref().expect("just opened");
    if !client.start_read() {
        return;
    }
    loop {
        let handles = [client.read_event, o.bell_game()];
        // SAFETY: waits on two handles this thread owns; both sources are polled below.
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };

        let bits = o.take_to_game();
        if bits != 0 {
            match o.lock(LOCK_TIMEOUT_MS) {
                Ok(g) => {
                    let s = state::read(g.bytes());
                    drop(g);
                    on_bells(&Link { objects: o, seen }, bits, &s);
                }
                // The helper holds the lock: put the bits back and ring again, so the next pass
                // retries; they are never lost.
                Err(e) => {
                    eprintln!("[uw-game-channel] bell: state lock {e:?}; retrying");
                    o.ring_game(bits);
                }
            }
        }

        match client.poll_read() {
            Read::Pending => {}
            Read::Message(bytes) => match decode_overlay(&bytes) {
                Ok(OverlayMessage::Ping { nonce }) => {
                    if !client.write(&encode_overlay(&OverlayMessage::Pong { nonce, seen_seq: seen.get() }))
                        || !client.start_read()
                    {
                        return;
                    }
                }
                // Anything else from the helper is a bad frame: drop the connection.
                _ => return,
            },
            Read::Broken => return,
        }
    }
}

enum Read {
    Pending,
    Message(Vec<u8>),
    Broken,
}

/// The overlay-pipe client: an overlapped, message-mode handle with one read pending at a time.
/// Dropping it cancels the read (and waits for the cancel) and closes everything, also on unwind.
struct Client {
    pipe: HANDLE,
    read_event: HANDLE,
    write_event: HANDLE,
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    reading: bool,
}

fn manual_event() -> HANDLE {
    // SAFETY: an unnamed, manual-reset, initially unsignaled event.
    unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
}

impl Client {
    /// `None` while the pipe is missing or busy (both mean "retry later").
    fn connect(name: &str) -> Option<Client> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated name; opens an existing pipe for overlapped duplex I/O.
        let pipe = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return None;
        }
        let client = Client {
            pipe,
            read_event: manual_event(),
            write_event: manual_event(),
            // SAFETY: OVERLAPPED is plain data; all-zero is its initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            reading: false,
        };
        if client.read_event.is_null() || client.write_event.is_null() {
            return None; // `client` drops: handles closed
        }
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: switch our end to message read mode so one read yields one message.
        if unsafe { SetNamedPipeHandleState(client.pipe, &mode, std::ptr::null(), std::ptr::null()) } == 0 {
            return None;
        }
        Some(client)
    }

    /// Start reading the next message. `false`: the pipe is gone.
    fn start_read(&mut self) -> bool {
        // SAFETY: an overlapped read into `buf` (never resized) on the boxed `ov`; both live in
        // `self` until the read completes or `Drop` cancels it.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.read_event;
            ResetEvent(self.read_event);
            let ok = ReadFile(self.pipe, self.buf.as_mut_ptr(), self.buf.len() as u32, std::ptr::null_mut(), &mut *self.ov);
            if ok != 0 { 0 } else { GetLastError() }
        };
        self.reading = err == 0 || err == ERROR_IO_PENDING;
        self.reading
    }

    fn poll_read(&mut self) -> Read {
        if !self.reading {
            return Read::Broken;
        }
        let mut n = 0u32;
        // SAFETY: collects the pending read's result without waiting.
        unsafe {
            if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                self.reading = false;
                return if n > 0 { Read::Message(self.buf[..n as usize].to_vec()) } else { Read::Broken };
            }
            if GetLastError() == ERROR_IO_INCOMPLETE {
                return Read::Pending;
            }
        }
        self.reading = false;
        Read::Broken
    }

    /// Write one message, waiting at most `WRITE_TIMEOUT_MS`. `false`: the pipe is gone.
    fn write(&self, bytes: &[u8]) -> bool {
        // SAFETY: an overlapped write with its own event; on a timeout it is cancelled and the
        // cancel awaited before `ov` and `bytes` go out of scope.
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.write_event;
            ResetEvent(self.write_event);
            let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
            let mut n = 0u32;
            if WaitForSingleObject(self.write_event, WRITE_TIMEOUT_MS) != WAIT_OBJECT_0 {
                CancelIoEx(self.pipe, &ov);
                GetOverlappedResult(self.pipe, &ov, &mut n, 1);
                return false;
            }
            GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: cancels our pending read and waits for it before `ov` and `buf` are freed, then
        // closes the handles this client owns.
        unsafe {
            if self.reading {
                CancelIoEx(self.pipe, &*self.ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, &*self.ov, &mut n, 1);
            }
            for h in [self.pipe, self.read_event, self.write_event] {
                if !h.is_null() {
                    CloseHandle(h);
                }
            }
        }
    }
}
