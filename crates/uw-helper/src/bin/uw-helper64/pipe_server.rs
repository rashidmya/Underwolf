//! The helper's pipe server: one overlapped, message-mode, local-only duplex pipe instance
//! (`FILE_FLAG_FIRST_PIPE_INSTANCE`, one instance), the accept state machine that `decide`'s
//! `wait_hello` slices drive, and bounded overlapped reads and writes for the relay.

use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_pipe_proto::pipe::{encode, Message, HEADER_LEN, MAX_PAYLOAD};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
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

/// How long the relay waits for one pipe write, and for the bootstrap's `status` reply.
pub(crate) const RELAY_TIMEOUT_MS: u64 = 15_000;

fn now_ms() -> u64 {
    // SAFETY: a monotonic tick count.
    unsafe { GetTickCount64() }
}

/// Where the client's accept stands between `accept_hello` slices. `decide` calls `wait_hello`
/// in slices of at most `REWAKE_MS`, so a slice that times out leaves the overlapped connect or
/// read pending on `PipeServer::ov` for the next slice: it is never cancelled mid-wait and the
/// client's connection is never dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accept {
    /// No I/O pending and no client: the next slice starts an overlapped `ConnectNamedPipe`.
    Listening,
    /// An overlapped `ConnectNamedPipe` is pending on `ov`.
    Connecting,
    /// A client is connected; an overlapped `ReadFile` of its first message is pending on
    /// `ov`, into `buf`.
    Reading,
    /// The first message was read; the pipe now belongs to the relay.
    Connected,
}

pub(crate) struct PipeServer {
    pipe: HANDLE,
    /// Manual-reset event for every overlapped operation (one at a time).
    event: HANDLE,
    /// The accept's `OVERLAPPED`, boxed so its address stays fixed while I/O is pending.
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    accept: Accept,
}

impl PipeServer {
    /// Create the pipe instance and its event. `name` is a `\\.\pipe\…` name.
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
        // SAFETY: a manual-reset, initially unsignaled event for overlapped I/O.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            // SAFETY: closes the pipe just created.
            unsafe { CloseHandle(pipe) };
            return Err("CreateEventW failed".into());
        }
        Ok(PipeServer {
            pipe,
            event,
            // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            accept: Accept::Listening,
        })
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

    /// Start the overlapped read of the client's first message into `buf`.
    fn start_read(&mut self) -> Option<HelloWait> {
        // SAFETY: an overlapped read into `buf` (never resized) whose `OVERLAPPED` is
        // `self.ov`; both live in `self` until the read completes or is cancelled.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(
                self.pipe,
                self.buf.as_mut_ptr(),
                self.buf.len() as u32,
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

    /// Classify the `n`-byte first message in `buf`; the pipe then belongs to the relay.
    fn take_hello(&mut self, n: u32) -> HelloWait {
        if n == 0 {
            self.drop_client();
            return HelloWait::Timeout;
        }
        self.accept = Accept::Connected;
        // Portable and unit-tested; maps another header version to `version_mismatch`.
        hello_wait_from_bytes(&self.buf[..n as usize])
    }

    /// Wait for the pending op on `ov` until `deadline`: `Ok(None)` while it is still
    /// pending (it stays pending), `Ok(Some(bytes))` once it completed, `Err` if it failed.
    fn wait_pending(&mut self, deadline: u64) -> Result<Option<u32>, u32> {
        let left = deadline.saturating_sub(now_ms());
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
    pub(crate) fn accept_hello(&mut self, timeout_ms: u64) -> HelloWait {
        let deadline = now_ms().saturating_add(timeout_ms);
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
    pub(crate) fn read_message(&self, timeout_ms: u64) -> Option<Vec<u8>> {
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

    pub(crate) fn write_message(&self, m: &Message) -> bool {
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
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // A pending accept still writes into `ov` and `buf`: cancel it and wait for the cancel
        // before they are freed (plan-review I5).
        if matches!(self.accept, Accept::Connecting | Accept::Reading) {
            self.cancel(&*self.ov);
        }
        for h in [self.pipe, self.event] {
            // SAFETY: closes handles this server created and still owns.
            unsafe { CloseHandle(h) };
        }
    }
}
