//! Named Win32 objects for the channel (`Local\uw-gep-<channel>-*`): file mappings for the hello
//! block, info store and event ring, a mutex per region, `connect`/`disconnect` manual-reset events
//! and `info`/`events` auto-reset signals. The producer lives in the game, the consumer in the helper.

use std::io;
use std::slice;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcessId, OpenEventW, OpenMutexW, ReleaseMutex, ResetEvent,
    SetEvent, SYNCHRONIZATION_ACCESS_RIGHTS, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::PCWSTR;

use crate::{
    FORMAT_VERSION, InfoReader, InfoWriter, Schema, info_region_size, rd_u32, ring_init, ring_region_size, wr_u32,
};

const SYNCHRONIZE: u32 = 0x0010_0000;
const EVENT_MODIFY_STATE: u32 = 0x0002;
const MUTEX_MODIFY_STATE: u32 = 0x0001;
const HELLO_SIZE: usize = 8;
/// How long the helper waits for a region mutex before giving up on one read.
const CONSUMER_LOCK_MS: u32 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Info,
    Events,
    Disconnect,
    Timeout,
}

fn name(channel: u32, suffix: &str) -> Vec<u16> {
    format!("Local\\uw-gep-{channel}-{suffix}").encode_utf16().chain(Some(0)).collect()
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct View {
    ptr: *mut u8,
    len: usize,
}

impl View {
    fn map(mapping: &Handle, access: windows::Win32::System::Memory::FILE_MAP) -> io::Result<View> {
        unsafe {
            let addr = MapViewOfFile(mapping.0, access, 0, 0, 0);
            if addr.Value.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let ret = VirtualQuery(Some(addr.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            if ret == 0 {
                let err = io::Error::last_os_error();
                let _ = UnmapViewOfFile(addr);
                return Err(err);
            }
            Ok(View { ptr: addr.Value.cast(), len: info.RegionSize })
        }
    }

    fn bytes(&self, len: usize) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr, len.min(self.len)) }
    }

    #[allow(clippy::mut_from_ref)]
    fn bytes_mut(&self, len: usize) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr, len.min(self.len)) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr.cast() });
        }
    }
}

struct Guard<'a>(&'a Handle);

impl<'a> Guard<'a> {
    /// Waits up to `timeout_ms` for the mutex. An abandoned mutex (previous owner died) is acquired;
    /// a timeout or failure yields `None`, with nothing held.
    fn try_lock(mutex: &'a Handle, timeout_ms: u32) -> Option<Guard<'a>> {
        let r = unsafe { WaitForSingleObject(mutex.0, timeout_ms) };
        (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(Guard(mutex))
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0.0);
        }
    }
}

fn already_exists() -> bool {
    unsafe { GetLastError() == ERROR_ALREADY_EXISTS }
}

fn create_mapping(channel: u32, suffix: &str, size: usize) -> io::Result<(Handle, View)> {
    let n = name(channel, suffix);
    let h = unsafe {
        CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, (size as u64 >> 32) as u32, size as u32, PCWSTR(n.as_ptr()))?
    };
    let h = Handle(h);
    if already_exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("uw-gep-{channel}-{suffix} already exists")));
    }
    let view = View::map(&h, FILE_MAP_WRITE)?;
    Ok((h, view))
}

fn create_mutex(channel: u32, suffix: &str) -> io::Result<Handle> {
    let n = name(channel, suffix);
    let h = Handle(unsafe { CreateMutexW(None, false, PCWSTR(n.as_ptr()))? });
    if already_exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("uw-gep-{channel}-{suffix} already exists")));
    }
    Ok(h)
}

fn create_event(channel: u32, suffix: &str, manual_reset: bool) -> io::Result<Handle> {
    let n = name(channel, suffix);
    let h = Handle(unsafe { CreateEventW(None, manual_reset, false, PCWSTR(n.as_ptr()))? });
    if already_exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("uw-gep-{channel}-{suffix} already exists")));
    }
    Ok(h)
}

fn open_mapping(channel: u32, suffix: &str) -> io::Result<(Handle, View)> {
    let n = name(channel, suffix);
    let h = Handle(unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(n.as_ptr()))? });
    let view = View::map(&h, FILE_MAP_READ)?;
    Ok((h, view))
}

fn open_mutex(channel: u32, suffix: &str) -> io::Result<Handle> {
    let n = name(channel, suffix);
    Ok(Handle(unsafe { OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(SYNCHRONIZE | MUTEX_MODIFY_STATE), false, PCWSTR(n.as_ptr()))? }))
}

fn open_event(channel: u32, suffix: &str, modify: bool) -> io::Result<Handle> {
    let n = name(channel, suffix);
    let access = SYNCHRONIZE | if modify { EVENT_MODIFY_STATE } else { 0 };
    Ok(Handle(unsafe { OpenEventW(SYNCHRONIZATION_ACCESS_RIGHTS(access), false, PCWSTR(n.as_ptr()))? }))
}

pub struct Producer {
    info_size: usize,
    ring_size: usize,
    _hello: (Handle, View),
    info: (Handle, View),
    events: (Handle, View),
    info_mutex: Handle,
    events_mutex: Handle,
    connect: Handle,
    disconnect: Handle,
    info_signal: Handle,
    events_signal: Handle,
}

// The raw view pointers refer to process-wide mappings; all access goes through `&self`/`&mut self`.
unsafe impl Send for Producer {}

impl Producer {
    /// Creates every object, initialises both regions and sets `connect`. Fails with `AlreadyExists`
    /// if a previous producer's objects are still open somewhere.
    pub fn create(channel: u32, schema: &Schema) -> io::Result<Producer> {
        let info_size = info_region_size(schema);
        let ring_size = ring_region_size();
        let hello = create_mapping(channel, "hello", HELLO_SIZE)?;
        let info = create_mapping(channel, "info", info_size)?;
        let events = create_mapping(channel, "events", ring_size)?;
        let p = Producer {
            info_size,
            ring_size,
            info_mutex: create_mutex(channel, "info-mutex")?,
            events_mutex: create_mutex(channel, "events-mutex")?,
            connect: create_event(channel, "connect", true)?,
            disconnect: create_event(channel, "disconnect", true)?,
            info_signal: create_event(channel, "info-signal", false)?,
            events_signal: create_event(channel, "events-signal", false)?,
            _hello: hello,
            info,
            events,
        };
        let h = p._hello.1.bytes_mut(HELLO_SIZE);
        wr_u32(h, 0, FORMAT_VERSION);
        wr_u32(h, 4, unsafe { GetCurrentProcessId() });
        InfoWriter::init(p.info.1.bytes_mut(info_size), schema).map_err(io::Error::other)?;
        ring_init(p.events.1.bytes_mut(ring_size));
        unsafe { SetEvent(p.connect.0)? };
        Ok(p)
    }

    /// Runs `f` on the info region under its mutex. Never blocks: if another process holds the
    /// mutex this returns `None` without running `f`, and the caller retries on its next pass.
    pub fn with_info<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        let _g = Guard::try_lock(&self.info_mutex, 0)?;
        Some(f(self.info.1.bytes_mut(self.info_size)))
    }

    /// Like `with_info`, for the event ring.
    pub fn with_events<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        let _g = Guard::try_lock(&self.events_mutex, 0)?;
        Some(f(self.events.1.bytes_mut(self.ring_size)))
    }

    pub fn signal_info(&self) {
        unsafe {
            let _ = SetEvent(self.info_signal.0);
        }
    }

    pub fn signal_events(&self) {
        unsafe {
            let _ = SetEvent(self.events_signal.0);
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.disconnect.0);
            let _ = ResetEvent(self.connect.0);
        }
    }
}

pub struct Consumer {
    hello: (u32, u32),
    info_size: usize,
    ring_size: usize,
    _hello: (Handle, View),
    info: (Handle, View),
    events: (Handle, View),
    info_mutex: Handle,
    events_mutex: Handle,
    disconnect: Handle,
    info_signal: Handle,
    events_signal: Handle,
}

unsafe impl Send for Consumer {}

impl Consumer {
    /// Opens a connected producer's objects. Fails with `NotFound`/`NotConnected` if there is none,
    /// and `InvalidData` if the hello block or regions are not in the expected format.
    pub fn open(channel: u32) -> io::Result<Consumer> {
        let connect = open_event(channel, "connect", false)?;
        if unsafe { WaitForSingleObject(connect.0, 0) } != WAIT_OBJECT_0 {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "producer not connected"));
        }
        let hello = open_mapping(channel, "hello")?;
        let (version, pid) = (rd_u32(hello.1.bytes(HELLO_SIZE), 0), rd_u32(hello.1.bytes(HELLO_SIZE), 4));
        if version != FORMAT_VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported hello version {version}")));
        }
        let info = open_mapping(channel, "info")?;
        let info_size = InfoReader::attach(info.1.bytes(info.1.len)).map_err(io::Error::other)?.region_size();
        let events = open_mapping(channel, "events")?;
        let ring_size = ring_region_size();
        if events.1.len < ring_size {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "event region too small"));
        }
        Ok(Consumer {
            hello: (version, pid),
            info_size,
            ring_size,
            info_mutex: open_mutex(channel, "info-mutex")?,
            events_mutex: open_mutex(channel, "events-mutex")?,
            disconnect: open_event(channel, "disconnect", false)?,
            info_signal: open_event(channel, "info-signal", false)?,
            events_signal: open_event(channel, "events-signal", false)?,
            _hello: hello,
            info,
            events,
        })
    }

    /// `(format version, producer pid)` from the hello block.
    pub fn hello(&self) -> (u32, u32) {
        self.hello
    }

    /// Holds the info mutex while `f` runs, so keep `f` short (copy out, then process). Waits up to
    /// 2 s for the mutex; `None` (timeout or wait failure) means `f` did not run.
    pub fn with_info<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let _g = Guard::try_lock(&self.info_mutex, CONSUMER_LOCK_MS)?;
        Some(f(self.info.1.bytes(self.info_size)))
    }

    /// Like `with_info`, for the event ring.
    pub fn with_events<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let _g = Guard::try_lock(&self.events_mutex, CONSUMER_LOCK_MS)?;
        Some(f(self.events.1.bytes(self.ring_size)))
    }

    /// Waits for new info, new events, or the producer going away. `Disconnect` wins ties.
    pub fn wait(&self, timeout_ms: u32) -> Wake {
        let handles = [self.disconnect.0, self.info_signal.0, self.events_signal.0];
        let r = unsafe { WaitForMultipleObjects(&handles, false, timeout_ms) };
        match r {
            r if r == WAIT_OBJECT_0 => Wake::Disconnect,
            r if r.0 == WAIT_OBJECT_0.0 + 1 => Wake::Info,
            r if r.0 == WAIT_OBJECT_0.0 + 2 => Wake::Events,
            r if r == WAIT_TIMEOUT => Wake::Timeout,
            _ => Wake::Disconnect,
        }
    }
}
