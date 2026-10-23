//! The helper's pipe server, used for both the bootstrap pipe and the overlay pipe: one
//! overlapped, message-mode, local-only duplex pipe instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`,
//! one instance). `poll` advances its accept-and-read state machine without blocking: there is
//! always one connect or one read pending on `ov`, signalled through `event`, so the relay can
//! wait on every pipe at once and `decide`'s `wait_hello` can wait in slices without ever
//! dropping a client. Writes use their own event and `OVERLAPPED`, so they never disturb the
//! pending read.

use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_pipe_proto::pipe::{HEADER_LEN, MAX_PAYLOAD};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_CONNECTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::{wait_ms, wide};

/// How long one pipe write may take, and the default `status` and `ping` timeouts.
pub(crate) const RELAY_TIMEOUT_MS: u64 = 15_000;

pub(crate) fn now_ms() -> u64 {
    // SAFETY: a monotonic tick count.
    unsafe { GetTickCount64() }
}

/// What `poll` saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PipeEvent {
    /// A client connected (its first message comes next).
    Connected,
    /// One whole message from the client.
    Message(Vec<u8>),
    /// The client left; the server listens again.
    Disconnected,
    /// The client sent a message over the frame limit (header + `MAX_PAYLOAD`): a bad message
    /// (design: `pipe_error`). The client is dropped and the server listens again.
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing pending: the next `poll` starts an overlapped `ConnectNamedPipe`.
    Listening,
    /// A `ConnectNamedPipe` is pending on `ov`.
    Connecting,
    /// A client is connected and a `ReadFile` into `buf` is pending on `ov`.
    Reading,
    /// A read failed to start: the client is gone; the next `poll` reports `Disconnected`.
    Broken,
    /// A read completed at once with more data than `buf` holds; the next `poll` reports
    /// `Oversized`.
    Overflowed,
}

/// How the operation on `ov` stands.
enum Io {
    Pending,
    Done(u32),
    /// A read whose message is larger than `buf` (`ERROR_MORE_DATA`).
    TooLarge,
    Failed,
}

pub(crate) struct PipeServer {
    pipe: HANDLE,
    /// Manual-reset event of the connect or read pending on `ov`.
    event: HANDLE,
    /// Boxed so its address stays fixed while the I/O is pending.
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    state: State,
    /// Manual-reset event for writes.
    write_event: HANDLE,
}

fn manual_event() -> HANDLE {
    // SAFETY: an unnamed, manual-reset, initially unsignaled event for overlapped I/O.
    unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
}

impl PipeServer {
    /// Create the pipe instance and its events. `name` is a `\\.\pipe\…` name.
    pub(crate) fn create(name: &str) -> Result<PipeServer, String> {
        let name = wide(name);
        // SAFETY: one overlapped, first-instance, message-mode, local-only duplex pipe.
        let pipe = unsafe {
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
        if pipe == INVALID_HANDLE_VALUE {
            // SAFETY: no arguments.
            return Err(format!("CreateNamedPipeW: {}", unsafe { GetLastError() }));
        }
        let (event, write_event) = (manual_event(), manual_event());
        if event.is_null() || write_event.is_null() {
            for h in [pipe, event, write_event] {
                if !h.is_null() {
                    // SAFETY: closes a handle just created here.
                    unsafe { CloseHandle(h) };
                }
            }
            return Err("CreateEventW failed".into());
        }
        Ok(PipeServer {
            pipe,
            event,
            // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            state: State::Listening,
            write_event,
        })
    }

    /// Signalled when `poll` may have something: wait on it, then call `poll`.
    pub(crate) fn event(&self) -> HANDLE {
        self.event
    }

    /// Advance without blocking: what happened, or `None` while the pending I/O is still pending.
    pub(crate) fn poll(&mut self) -> Option<PipeEvent> {
        loop {
            match self.state {
                State::Listening => {
                    // SAFETY: an overlapped connect on `ov` (boxed, owned by `self`), its event
                    // reset first.
                    let err = unsafe {
                        *self.ov = std::mem::zeroed();
                        self.ov.hEvent = self.event;
                        ResetEvent(self.event);
                        if ConnectNamedPipe(self.pipe, &mut *self.ov) != 0 { 0 } else { GetLastError() }
                    };
                    match err {
                        ERROR_IO_PENDING => {
                            self.state = State::Connecting;
                            return None;
                        }
                        // Connected already (the client won the race with this call).
                        0 | ERROR_PIPE_CONNECTED => {
                            self.start_read();
                            return Some(PipeEvent::Connected);
                        }
                        // A client that came and went (`ERROR_NO_DATA`) or another failure: free
                        // the instance and try again on the next wake-up (`event` is reset, so
                        // this does not spin).
                        e => {
                            eprintln!("[uw-helper64] ConnectNamedPipe: {e}");
                            // SAFETY: disconnects our instance; no I/O is pending on it.
                            unsafe { DisconnectNamedPipe(self.pipe) };
                            return None;
                        }
                    }
                }
                State::Connecting => match self.result() {
                    Io::Pending => return None,
                    Io::Done(_) => {
                        self.start_read();
                        return Some(PipeEvent::Connected);
                    }
                    // The client connected and left already: accept again.
                    Io::TooLarge | Io::Failed => self.reset(),
                },
                State::Reading => match self.result() {
                    Io::Pending => return None,
                    Io::Done(n) if n > 0 => {
                        let msg = self.buf[..n as usize].to_vec();
                        self.start_read();
                        return Some(PipeEvent::Message(msg));
                    }
                    Io::TooLarge => {
                        self.reset();
                        return Some(PipeEvent::Oversized);
                    }
                    // A zero-length message or a closed client.
                    Io::Done(_) | Io::Failed => {
                        self.reset();
                        return Some(PipeEvent::Disconnected);
                    }
                },
                State::Broken => {
                    self.reset();
                    return Some(PipeEvent::Disconnected);
                }
                State::Overflowed => {
                    self.reset();
                    return Some(PipeEvent::Oversized);
                }
            }
        }
    }

    /// `poll`, waiting up to `timeout_ms` for something to happen.
    pub(crate) fn wait(&mut self, timeout_ms: u64) -> Option<PipeEvent> {
        let deadline = now_ms().saturating_add(timeout_ms);
        loop {
            if let Some(e) = self.poll() {
                return Some(e);
            }
            let left = deadline.saturating_sub(now_ms());
            // SAFETY: waits on our event.
            if left == 0 || unsafe { WaitForSingleObject(self.event, wait_ms(left)) } != WAIT_OBJECT_0 {
                return None;
            }
        }
    }

    /// One `decide::wait_hello` slice: accept a client and read its first message within
    /// `timeout_ms`. A timeout leaves the connect or read pending for the next slice; a client
    /// that leaves before its first message is dropped and the next one accepted.
    pub(crate) fn accept_hello(&mut self, timeout_ms: u64) -> HelloWait {
        let deadline = now_ms().saturating_add(timeout_ms);
        loop {
            match self.wait(deadline.saturating_sub(now_ms())) {
                Some(PipeEvent::Message(bytes)) => return hello_wait_from_bytes(&bytes),
                // A client that left, or sent an oversized first message, is dropped: accept again.
                Some(PipeEvent::Connected | PipeEvent::Disconnected | PipeEvent::Oversized) => continue,
                None => return HelloWait::Timeout,
            }
        }
    }

    /// Drop the client (after a bad message): cancel the pending read and listen again.
    pub(crate) fn disconnect(&mut self) {
        if matches!(self.state, State::Connecting | State::Reading) {
            self.cancel();
        }
        self.reset();
    }

    /// Write one message, waiting at most `RELAY_TIMEOUT_MS`. `false`: the client is gone.
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
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
            if WaitForSingleObject(self.write_event, wait_ms(RELAY_TIMEOUT_MS)) != WAIT_OBJECT_0 {
                CancelIoEx(self.pipe, &ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, &ov, &mut n, 1);
                return false;
            }
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
        }
    }

    /// Start the read of the client's next message into `buf`. Its completion, now or later, is
    /// collected by the next `poll`.
    fn start_read(&mut self) {
        // SAFETY: an overlapped read into `buf` (never resized) on `ov`; both live in `self`
        // until the read completes or is cancelled.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(self.pipe, self.buf.as_mut_ptr(), self.buf.len() as u32, std::ptr::null_mut(), &mut *self.ov);
            if ok != 0 { 0 } else { GetLastError() }
        };
        self.state = match err {
            0 | ERROR_IO_PENDING => State::Reading,
            ERROR_MORE_DATA => State::Overflowed,
            _ => State::Broken,
        };
    }

    fn result(&self) -> Io {
        let mut n = 0u32;
        // SAFETY: collects `ov`'s result without waiting.
        unsafe {
            if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                return Io::Done(n);
            }
            match GetLastError() {
                ERROR_IO_INCOMPLETE => Io::Pending,
                ERROR_MORE_DATA => Io::TooLarge,
                _ => Io::Failed,
            }
        }
    }

    /// Cancel the I/O pending on `ov` and wait for the cancel, so `ov` and `buf` are free again.
    fn cancel(&self) {
        // SAFETY: cancels I/O on our pipe and blocks until the cancel completes.
        unsafe {
            CancelIoEx(self.pipe, &*self.ov);
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, &*self.ov, &mut n, 1);
        }
    }

    /// Free the instance for the next client; nothing may be pending on `ov`.
    fn reset(&mut self) {
        // SAFETY: disconnects our instance and resets our event.
        unsafe {
            DisconnectNamedPipe(self.pipe);
            ResetEvent(self.event);
        }
        self.state = State::Listening;
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // A pending connect or read still writes into `ov` and `buf`: cancel it and wait for the
        // cancel before they are freed (plan 1 plan-review I5).
        if matches!(self.state, State::Connecting | State::Reading) {
            self.cancel();
        }
        for h in [self.pipe, self.event, self.write_event] {
            // SAFETY: closes handles this server created and still owns.
            unsafe { CloseHandle(h) };
        }
    }
}
