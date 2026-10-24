//! The game-data channel's named objects (design 2b "Components and flow"), for channel
//! `<channel>` of the game `<pid>` (names from `uw_pipe_proto::names::data_name`): three
//! paging-file mappings (`hello`, `info`, `events`), a lock and a bell for each region, and the
//! `connect`/`disconnect` manual-reset events. The layouts are `uw_pipe_proto::data`.
//!
//! - `Producer`, in the game, creates every object, owns the schema and writes info and events
//!   from one thread of its own (`&mut self`). It never waits on a shared lock: it tries with
//!   0 ms, and a busy lock leaves the work pending for the next call.
//! - `Consumer`, the helper's data thread, opens the objects, maps both regions read-only, waits
//!   at most `LOCK_TIMEOUT_MS` for a lock and checks every read. A lock whose owner died holding
//!   it (`WAIT_ABANDONED`) counts as acquired: every read re-checks the headers anyway.

use uw_pipe_proto::data::{
    Drained, HELLO_SIZE, InfoReader, InfoValue, Outbox, ProduceError, RING_SIZE, ReadError,
    ResumePoint, RingReader, Schema, info_init, read_hello, ring_init, write_hello,
};
use uw_pipe_proto::names::{DataObject, data_name};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, OpenFileMappingW, PAGE_READWRITE,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcessId, MUTEX_MODIFY_STATE, OpenEventW, OpenMutexW,
    ReleaseMutex, ResetEvent, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, Sleep, WaitForMultipleObjects,
    WaitForSingleObject,
};

use crate::objects::{Handle, LOCK_TIMEOUT_MS, ObjectsError, View, last_error, wide};

/// A Win32 call that failed: which call on which object, and its `GetLastError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win32Error {
    pub what: String,
    pub code: u32,
}

impl std::fmt::Display for Win32Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: error {}", self.what, self.code)
    }
}

fn win32(call: &str, name: &str) -> Win32Error {
    Win32Error {
        what: format!("{call} {name}"),
        code: last_error(),
    }
}

impl From<ObjectsError> for Win32Error {
    fn from(e: ObjectsError) -> Win32Error {
        match e {
            ObjectsError::Win32 { what, code } => Win32Error { what, code },
            other => Win32Error {
                what: other.to_string(),
                code: 0,
            },
        }
    }
}

/// Why `Producer::create` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// An object of this channel already exists: another producer's, or an earlier one's that a
    /// helper still holds because it has not seen `disconnect` yet (design "Risks").
    Exists(String),
    Win32(Win32Error),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::Exists(name) => write!(f, "{name} already exists"),
            CreateError::Win32(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for CreateError {}

impl From<Win32Error> for CreateError {
    fn from(e: Win32Error) -> CreateError {
        CreateError::Win32(e)
    }
}

/// A mapping and the whole view of it.
struct Mapping {
    _handle: Handle,
    view: View,
}

impl Mapping {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the view is mapped for `len` bytes while `self` lives. The producer writes the
        // hello block before it sets `connect`, and both regions only under their locks, which
        // every reader of these bytes holds.
        unsafe { std::slice::from_raw_parts(self.view.ptr, self.view.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, on a read/write view; `&mut self` makes this the only borrow in this
        // process, and the caller holds the region's lock (or no one else can open it yet).
        unsafe { std::slice::from_raw_parts_mut(self.view.ptr, self.view.len) }
    }
}

/// A lock held until drop.
struct Locked<'a>(&'a Handle);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: releases the mutex this thread acquired for this guard.
        unsafe { ReleaseMutex(self.0.0) };
    }
}

/// Take `lock` within `timeout_ms`. An abandoned lock counts as acquired (design "Failures"):
/// its owner died holding it, and the reads that follow re-check the headers.
fn lock(mutex: &Handle, timeout_ms: u32) -> Option<Locked<'_>> {
    // SAFETY: waits on our mutex handle.
    let r = unsafe { WaitForSingleObject(mutex.0, timeout_ms) };
    (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(Locked(mutex))
}

/// The `GetLastError` right after a `Create*W` that returned a handle: is it an existing object?
fn existed() -> bool {
    // SAFETY: no arguments; read right after the create call.
    unsafe { GetLastError() == ERROR_ALREADY_EXISTS }
}

fn new_mapping(name: &str, size: usize) -> Result<Mapping, CreateError> {
    let w = wide(name);
    // SAFETY: a named, paging-file-backed, read/write mapping of `size` bytes.
    let h = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE,
            (size as u64 >> 32) as u32,
            size as u32,
            w.as_ptr(),
        )
    };
    if h.is_null() {
        return Err(win32("CreateFileMappingW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    let view =
        View::map_as(&handle, FILE_MAP_READ | FILE_MAP_WRITE, name).map_err(Win32Error::from)?;
    if view.len < size {
        return Err(Win32Error {
            what: format!("{name} maps {} bytes, under {size}", view.len),
            code: 0,
        }
        .into());
    }
    Ok(Mapping {
        _handle: handle,
        view,
    })
}

fn new_mutex(name: &str) -> Result<Handle, CreateError> {
    let w = wide(name);
    // SAFETY: a named mutex, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("CreateMutexW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    Ok(handle)
}

fn new_event(name: &str, manual_reset: bool) -> Result<Handle, CreateError> {
    let w = wide(name);
    // SAFETY: a named event, initially unsignaled; manual-reset or auto-reset as asked.
    let h = unsafe { CreateEventW(std::ptr::null(), manual_reset as i32, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("CreateEventW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    Ok(handle)
}

/// The producer's clock in ms (design: `ts` is `QueryPerformanceCounter`).
struct Clock {
    per_second: i64,
}

impl Clock {
    fn new() -> Clock {
        let mut f = 0i64;
        // SAFETY: writes one i64; it never fails on Windows XP and later.
        unsafe { QueryPerformanceFrequency(&mut f) };
        Clock {
            per_second: f.max(1),
        }
    }

    fn now_ms(&self) -> u64 {
        let mut c = 0i64;
        // SAFETY: writes one i64.
        unsafe { QueryPerformanceCounter(&mut c) };
        (c.max(0) as u128 * 1000 / self.per_second as u128) as u64
    }
}

/// The in-game side of one channel. Dropping it sets `disconnect` and resets `connect`.
pub struct Producer {
    outbox: Outbox,
    producer_id: u64,
    clock: Clock,
    _hello: Mapping,
    info: Mapping,
    events: Mapping,
    info_lock: Handle,
    events_lock: Handle,
    info_bell: Handle,
    events_bell: Handle,
    disconnect: Handle,
    connect: Handle,
}

impl Producer {
    /// Create channel `channel` of this process (design flow step 1), in order: the hello block,
    /// the info store with its schema written in, the event ring, both locks, both bells,
    /// `disconnect` and then `connect`. The hello block is written last, then `connect` is set.
    /// If any object already exists, it fails with `Exists`.
    pub fn create(channel: u32, schema: Schema) -> Result<Producer, CreateError> {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let name = |o| data_name(pid, channel, o);
        let outbox = Outbox::new(schema);
        let mut hello = new_mapping(&name(DataObject::Hello), HELLO_SIZE)?;
        let mut info = new_mapping(&name(DataObject::Info), outbox.info_layout().size())?;
        // No helper reads before `connect` is set, so the regions are written without the locks.
        info_init(info.bytes_mut(), outbox.schema(), outbox.info_layout());
        let mut events = new_mapping(&name(DataObject::Events), RING_SIZE)?;
        ring_init(events.bytes_mut());
        let info_lock = new_mutex(&name(DataObject::InfoLock))?;
        let events_lock = new_mutex(&name(DataObject::EventsLock))?;
        let info_bell = new_event(&name(DataObject::InfoBell), false)?;
        let events_bell = new_event(&name(DataObject::EventsBell), false)?;
        let disconnect = new_event(&name(DataObject::Disconnect), true)?;
        let connect = new_event(&name(DataObject::Connect), true)?;
        // SAFETY: no arguments.
        let producer_id = unsafe { GetTickCount64() };
        write_hello(hello.bytes_mut(), pid, producer_id);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(connect.0) };
        Ok(Producer {
            outbox,
            producer_id,
            clock: Clock::new(),
            _hello: hello,
            info,
            events,
            info_lock,
            events_lock,
            info_bell,
            events_bell,
            disconnect,
            connect,
        })
    }

    /// This producer's id (the hello block's `producer_id`).
    pub fn producer_id(&self) -> u64 {
        self.producer_id
    }

    /// One info transaction: `(category, key, value)` entries, `None` for null. It is written at
    /// once if the info lock is free (then `info-bell` rings once, if anything changed), and on a
    /// later `commit` or `flush` otherwise.
    pub fn commit(&mut self, entries: &[(&str, &str, Option<&str>)]) -> Result<(), ProduceError> {
        self.outbox.commit(entries, self.clock.now_ms())?;
        self.write_info();
        Ok(())
    }

    /// Queue an event; `flush` writes it.
    pub fn push_event(&mut self, name: &str, data: &str) -> Result<(), ProduceError> {
        self.outbox.push_event(name, data, self.clock.now_ms())
    }

    /// Write pending info and queued events, within the throttle. Call it regularly.
    pub fn flush(&mut self) {
        self.write_info();
        self.write_events();
    }

    /// Events waiting for the ring.
    pub fn queued(&self) -> usize {
        self.outbox.queued()
    }

    fn write_info(&mut self) {
        if !self.outbox.has_pending_info() {
            return;
        }
        let Some(guard) = lock(&self.info_lock, 0) else {
            return;
        };
        let changed = self.outbox.write_info(self.info.bytes_mut());
        drop(guard);
        if changed {
            // SAFETY: signals our event handle.
            unsafe { SetEvent(self.info_bell.0) };
        }
    }

    fn write_events(&mut self) {
        if !self.outbox.has_pending_events() {
            return;
        }
        let Some(guard) = lock(&self.events_lock, 0) else {
            return;
        };
        let changed = self
            .outbox
            .write_events(self.events.bytes_mut(), self.clock.now_ms());
        drop(guard);
        if changed {
            // SAFETY: signals our event handle.
            unsafe { SetEvent(self.events_bell.0) };
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        // Reset `connect` first: a consumer woken by `disconnect` must already find it reset.
        // SAFETY: sets and resets our own event handles; the handles close after this.
        unsafe {
            ResetEvent(self.connect.0);
            SetEvent(self.disconnect.0);
        }
    }
}

/// Why `Consumer::open` did not open the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No producer yet: `connect` does not exist or is not set. Retry later.
    NotReady,
    Win32(Win32Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotReady => f.write_str("no producer yet"),
            OpenError::Win32(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for OpenError {}

impl From<Win32Error> for OpenError {
    fn from(e: Win32Error) -> OpenError {
        OpenError::Win32(e)
    }
}

/// Why a consumer read gave nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeError {
    /// The lock was not acquired within `LOCK_TIMEOUT_MS`: skip, and retry on the next pass.
    Busy,
    /// The bytes do not fit (design "Failures"): report once, and wait for `disconnect`.
    Read(ReadError),
}

impl std::fmt::Display for ConsumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsumeError::Busy => f.write_str("lock busy"),
            ConsumeError::Read(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for ConsumeError {}

impl From<ReadError> for ConsumeError {
    fn from(e: ReadError) -> ConsumeError {
        ConsumeError::Read(e)
    }
}

/// What woke `Consumer::wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Disconnect,
    Info,
    Events,
    Timeout,
}

/// A checked attach (design flow steps 2-4): what `data_connected` and the snapshot report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub producer_id: u64,
    /// The seq after which events will be delivered.
    pub from_seq: u64,
    pub schema: Schema,
    /// Every key that has been set, nulls included.
    pub snapshot: Vec<InfoValue>,
}

/// The helper's side of one channel. Single-threaded: its data thread waits, reads and drops it.
pub struct Consumer {
    game_pid: u32,
    hello: Mapping,
    info: Mapping,
    events: Mapping,
    info_lock: Handle,
    events_lock: Handle,
    disconnect: Handle,
    info_bell: Handle,
    events_bell: Handle,
    _connect: Handle,
    reader: Option<InfoReader>,
    ring: Option<RingReader>,
}

fn open_event(name: &str) -> Result<Handle, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named event, to wait on it only.
    let h = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, w.as_ptr()) };
    if h.is_null() {
        Err(win32("OpenEventW", name))
    } else {
        Ok(Handle(h))
    }
}

fn open_mutex(name: &str) -> Result<Handle, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named mutex to wait on it and release it.
    let h = unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
            0,
            w.as_ptr(),
        )
    };
    if h.is_null() {
        Err(win32("OpenMutexW", name))
    } else {
        Ok(Handle(h))
    }
}

fn open_mapping(name: &str) -> Result<Mapping, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named mapping for reading only.
    let h = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("OpenFileMappingW", name));
    }
    let handle = Handle(h);
    let view = View::map_as(&handle, FILE_MAP_READ, name)?;
    Ok(Mapping {
        _handle: handle,
        view,
    })
}

impl Consumer {
    /// Open channel `channel` of the game `pid` once its producer is ready: `connect` must exist
    /// and be set (design flow step 2). Then every other object is opened, the three mappings
    /// are mapped read-only, and `connect` must still be set. Nothing is checked yet: see
    /// `attach`.
    pub fn open(pid: u32, channel: u32) -> Result<Consumer, OpenError> {
        let name = |o| data_name(pid, channel, o);
        let connect = match open_event(&name(DataObject::Connect)) {
            Ok(h) => h,
            Err(e) if e.code == ERROR_FILE_NOT_FOUND => return Err(OpenError::NotReady),
            Err(e) => return Err(e.into()),
        };
        // SAFETY: polls our event handle without waiting.
        if unsafe { WaitForSingleObject(connect.0, 0) } != WAIT_OBJECT_0 {
            return Err(OpenError::NotReady);
        }
        let hello = open_mapping(&name(DataObject::Hello))?;
        let info = open_mapping(&name(DataObject::Info))?;
        let events = open_mapping(&name(DataObject::Events))?;
        let info_lock = open_mutex(&name(DataObject::InfoLock))?;
        let events_lock = open_mutex(&name(DataObject::EventsLock))?;
        let disconnect = open_event(&name(DataObject::Disconnect))?;
        let info_bell = open_event(&name(DataObject::InfoBell))?;
        let events_bell = open_event(&name(DataObject::EventsBell))?;
        // Poll `connect` again. Its producer may have dropped while the objects above were
        // opened, and a new producer may have created some of them since (its own create then
        // fails at `connect`, which we still hold). `Producer::drop` resets `connect` before any
        // of its objects closes, so a `connect` still set here proves that every object opened
        // above belongs to that producer. If it is not set, drop them all and retry later.
        // SAFETY: polls our event handle without waiting.
        if unsafe { WaitForSingleObject(connect.0, 0) } != WAIT_OBJECT_0 {
            return Err(OpenError::NotReady);
        }
        Ok(Consumer {
            game_pid: pid,
            hello,
            info,
            events,
            info_lock,
            events_lock,
            disconnect,
            info_bell,
            events_bell,
            _connect: connect,
            reader: None,
            ring: None,
        })
    }

    /// Check the hello block (magic, version, the game's pid), then under the info lock check
    /// the store and take the snapshot, then under the event lock check the ring and pick where
    /// events start: after `resume` if it names this producer, else at the write head. Calling it
    /// again starts over.
    pub fn attach(&mut self, resume: Option<ResumePoint>) -> Result<Attached, ConsumeError> {
        let hello = read_hello(self.hello.bytes(), self.game_pid)?;
        let (reader, snapshot) = {
            let _g = lock(&self.info_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
            let region = self.info.bytes();
            let mut reader = InfoReader::attach(region)?;
            let snapshot = reader.snapshot(region)?;
            (reader, snapshot)
        };
        let (ring, from_seq) = {
            let _g = lock(&self.events_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
            RingReader::start(self.events.bytes(), hello.producer_id, resume)?
        };
        let schema = reader.schema().clone();
        self.reader = Some(reader);
        self.ring = Some(ring);
        Ok(Attached {
            producer_id: hello.producer_id,
            from_seq,
            schema,
            snapshot,
        })
    }

    /// Under the info lock: the keys whose timestamps moved. Empty before `attach`.
    pub fn changes(&mut self) -> Result<Vec<InfoValue>, ConsumeError> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(Vec::new());
        };
        let _g = lock(&self.info_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
        Ok(reader.changes(self.info.bytes())?)
    }

    /// Under the event lock: the events since the last drain, in seq order, and what was lost
    /// before them. Empty before `attach`.
    pub fn drain(&mut self) -> Result<Drained, ConsumeError> {
        let Some(ring) = self.ring.as_mut() else {
            return Ok(Drained::default());
        };
        let _g = lock(&self.events_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
        Ok(ring.drain(self.events.bytes())?)
    }

    /// Wait up to `timeout_ms` on `[disconnect, info-bell, events-bell]`. `disconnect` has the
    /// smallest index, so it wins a tie (design "Platform facts").
    pub fn wait(&self, timeout_ms: u32) -> Wake {
        let handles = [self.disconnect.0, self.info_bell.0, self.events_bell.0];
        // SAFETY: waits on three handles this consumer owns; they are closed only when it drops.
        let r = unsafe { WaitForMultipleObjects(3, handles.as_ptr(), 0, timeout_ms) };
        match r {
            WAIT_OBJECT_0 => Wake::Disconnect,
            r if r == WAIT_OBJECT_0 + 1 => Wake::Info,
            r if r == WAIT_OBJECT_0 + 2 => Wake::Events,
            WAIT_TIMEOUT => Wake::Timeout,
            _ => {
                // A failed wait: sleep the timeout instead of spinning, then poll as on a timeout.
                // SAFETY: a plain sleep.
                unsafe { Sleep(timeout_ms) };
                Wake::Timeout
            }
        }
    }
}
