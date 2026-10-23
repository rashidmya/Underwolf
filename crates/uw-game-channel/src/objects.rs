//! The channel's named kernel objects for one game (`<pid>` = the game's Windows pid, names from
//! `uw_pipe_proto::names`): the `bell-game` and `bell-host` auto-reset events, the 16-byte mask
//! region, the state block and its lock. The helper creates them (`Objects::create`, before its
//! pipes); the game only opens them (`Objects::open`, after its `overlay_hello`).
//!
//! The masks are atomics on the shared view: a sender ORs bits in and signals the event, a
//! receiver swaps the mask to 0, so ringing never waits. The state block is read and written only
//! through `StateGuard`, which holds the state lock.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use uw_pipe_proto::bells::{MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET};
use uw_pipe_proto::names::{bell_game_name, bell_host_name, bells_name, state_lock_name, state_name};
use uw_pipe_proto::state::{self, HeaderError, BLOCK_SIZE};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, ReleaseMutex, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, MUTEX_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};

/// How long the helper and the game's worker wait for the state lock (design: 100 ms, then the
/// command fails with `state_busy`). Game threads other than the worker use 0.
pub const LOCK_TIMEOUT_MS: u32 = 100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLastError() }
}

/// Why the objects could not be created or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectsError {
    /// The state block's header is not layout v1. For the helper: a block the game held across a
    /// helper restart, from another client version (design: `version_mismatch`, "restart the
    /// game"; it is never rewritten).
    VersionMismatch(HeaderError),
    /// The state lock was not acquired within `LOCK_TIMEOUT_MS`.
    Busy,
    /// A Win32 call failed: which call on which object, and its `GetLastError`.
    Win32 { what: String, code: u32 },
}

impl std::fmt::Display for ObjectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectsError::VersionMismatch(h) => write!(f, "{h}"),
            ObjectsError::Busy => write!(f, "state lock busy"),
            ObjectsError::Win32 { what, code } => write!(f, "{what}: error {code}"),
        }
    }
}
impl std::error::Error for ObjectsError {}

fn win32(call: &str, name: &str) -> ObjectsError {
    ObjectsError::Win32 { what: format!("{call} {name}"), code: last_error() }
}

/// Why `Objects::lock` did not return a guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    /// Not acquired within the timeout (design: `state_busy`), or this thread already holds a
    /// `StateGuard` from the same `Objects`.
    Busy,
    /// Acquired from an owner that died holding it (`WAIT_ABANDONED`), and the header no longer
    /// checks out. The lock is released again.
    BadHeader(HeaderError),
}

impl From<LockError> for ObjectsError {
    fn from(e: LockError) -> ObjectsError {
        match e {
            LockError::Busy => ObjectsError::Busy,
            LockError::BadHeader(h) => ObjectsError::VersionMismatch(h),
        }
    }
}

/// An owned kernel handle, closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: closes the handle this value owns, exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A mapped view of a whole file mapping, unmapped on drop. `len` is the mapped region from
/// `VirtualQuery` (whole pages, so at least the object's size).
struct View {
    ptr: *mut u8,
    len: usize,
}

impl View {
    fn map(mapping: &Handle, name: &str) -> Result<View, ObjectsError> {
        // SAFETY: maps the whole object read/write; `VirtualQuery` only describes our own view.
        unsafe {
            let addr = MapViewOfFile(mapping.0, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
            if addr.Value.is_null() {
                return Err(win32("MapViewOfFile", name));
            }
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(addr.Value, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
                let e = win32("VirtualQuery", name);
                UnmapViewOfFile(addr);
                return Err(e);
            }
            Ok(View { ptr: addr.Value.cast(), len: info.RegionSize })
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: unmaps the view this value owns, exactly once.
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr.cast() }) };
    }
}

/// One game's channel objects, as the helper created or the game opened them.
pub struct Objects {
    bell_game: Handle,
    bell_host: Handle,
    _bells_mapping: Handle,
    bells: View,
    lock: Handle,
    _state_mapping: Handle,
    state: View,
    /// A `StateGuard` is alive. The Win32 mutex is recursive, so without this a second `lock` on
    /// the thread that holds it would hand out a second guard (two aliasing `bytes_mut`).
    held: AtomicBool,
}

// SAFETY: kernel handles may be used from any thread; the masks are only touched through
// atomics, and the state block only through a `StateGuard`, which holds the cross-process lock.
unsafe impl Send for Objects {}
// SAFETY: as above; `&Objects` hands out no unsynchronised access.
unsafe impl Sync for Objects {}

fn create_event(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: an auto-reset (bManualReset = FALSE), initially unsignaled, named event. An event
    // that already exists is returned as is (design "Platform facts").
    let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("CreateEventW", name)) } else { Ok(Handle(h)) }
}

fn create_mutex(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: a named mutex, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) };
    if h.is_null() { Err(win32("CreateMutexW", name)) } else { Ok(Handle(h)) }
}

/// Create (or open, if it exists) a paging-file mapping of `size` bytes. Returns whether it
/// already existed: then it has its own current size and contents (design "Platform facts").
fn create_mapping(name: &str, size: usize) -> Result<(Handle, View, bool), ObjectsError> {
    let w = wide(name);
    // SAFETY: a named, paging-file-backed, read/write mapping; `GetLastError` is read right after.
    let (h, existed) = unsafe {
        let h = CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, size as u32, w.as_ptr());
        (h, GetLastError() == ERROR_ALREADY_EXISTS)
    };
    if h.is_null() {
        return Err(win32("CreateFileMappingW", name));
    }
    let h = Handle(h);
    let view = View::map(&h, name)?;
    if view.len < size {
        return Err(ObjectsError::Win32 { what: format!("{name} maps {} bytes, under {size}", view.len), code: 0 });
    }
    Ok((h, view, existed))
}

fn open_event(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named event to wait on it and to signal it.
    let h = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("OpenEventW", name)) } else { Ok(Handle(h)) }
}

fn open_mutex(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named mutex to wait on it and release it.
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("OpenMutexW", name)) } else { Ok(Handle(h)) }
}

fn open_mapping(name: &str, size: usize) -> Result<(Handle, View), ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named mapping for read/write.
    let h = unsafe { OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("OpenFileMappingW", name));
    }
    let h = Handle(h);
    let view = View::map(&h, name)?;
    if view.len < size {
        return Err(ObjectsError::Win32 { what: format!("{name} maps {} bytes, under {size}", view.len), code: 0 });
    }
    Ok((h, view))
}

impl Objects {
    /// The helper's side (design flow step 2): create, in order, the doorbell events, the mask
    /// region, the state lock and the state block. A new block gets the v1 header; a block that
    /// already exists (the game held it across a helper restart) is checked and, if it is not v1,
    /// refused with `VersionMismatch` without being written.
    pub fn create(pid: u32) -> Result<Objects, ObjectsError> {
        let bell_game = create_event(&bell_game_name(pid))?;
        let bell_host = create_event(&bell_host_name(pid))?;
        let (bells_mapping, bells, _) = create_mapping(&bells_name(pid), MASK_REGION_SIZE)?;
        let lock = create_mutex(&state_lock_name(pid))?;
        let (state_mapping, state, existed) = create_mapping(&state_name(pid), BLOCK_SIZE)?;
        let o = Objects {
            bell_game,
            bell_host,
            _bells_mapping: bells_mapping,
            bells,
            lock,
            _state_mapping: state_mapping,
            state,
            held: AtomicBool::new(false),
        };
        if existed {
            let g = o.lock_raw(LOCK_TIMEOUT_MS).ok_or(ObjectsError::Busy)?;
            state::check_header(g.bytes()).map_err(ObjectsError::VersionMismatch)?;
        } else {
            // Nobody else can have opened it yet: the game opens the objects only after its
            // `overlay_hello`, and the overlay pipe is created after this.
            let mut g = o.lock_raw(LOCK_TIMEOUT_MS).ok_or(ObjectsError::Busy)?;
            state::init_header(g.bytes_mut());
        }
        Ok(o)
    }

    /// The game's side (design flow step 3): open the objects the helper created, and check that
    /// the state block is layout v1.
    pub fn open(pid: u32) -> Result<Objects, ObjectsError> {
        let bell_game = open_event(&bell_game_name(pid))?;
        let bell_host = open_event(&bell_host_name(pid))?;
        let (bells_mapping, bells) = open_mapping(&bells_name(pid), MASK_REGION_SIZE)?;
        let lock = open_mutex(&state_lock_name(pid))?;
        let (state_mapping, state) = open_mapping(&state_name(pid), BLOCK_SIZE)?;
        let o = Objects {
            bell_game,
            bell_host,
            _bells_mapping: bells_mapping,
            bells,
            lock,
            _state_mapping: state_mapping,
            state,
            held: AtomicBool::new(false),
        };
        {
            let g = o.lock(LOCK_TIMEOUT_MS)?;
            state::check_header(g.bytes()).map_err(ObjectsError::VersionMismatch)?;
        }
        Ok(o)
    }

    fn mask(&self, offset: usize) -> &AtomicU64 {
        debug_assert!(offset + 8 <= MASK_REGION_SIZE && self.bells.len >= MASK_REGION_SIZE);
        // SAFETY: the view is page-aligned and at least MASK_REGION_SIZE bytes, so offsets 0 and 8
        // are in bounds and 8-byte aligned; it stays mapped while `self` lives; and every access
        // to these words, in every process, is atomic.
        unsafe { AtomicU64::from_ptr(self.bells.ptr.add(offset).cast::<u64>()) }
    }

    /// Host -> game: OR `bits` into `to_game`, then signal `bell-game`. Never waits.
    pub fn ring_game(&self, bits: u64) {
        self.mask(TO_GAME_OFFSET).fetch_or(bits, Ordering::AcqRel);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(self.bell_game.0) };
    }

    /// Game -> host: OR `bits` into `to_host`, then signal `bell-host`. Never waits, so any game
    /// thread may ring.
    pub fn ring_host(&self, bits: u64) {
        self.mask(TO_HOST_OFFSET).fetch_or(bits, Ordering::AcqRel);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(self.bell_host.0) };
    }

    /// The game's receive: swap `to_game` to 0 and return what was there.
    pub fn take_to_game(&self) -> u64 {
        self.mask(TO_GAME_OFFSET).swap(0, Ordering::AcqRel)
    }

    /// The helper's receive: swap `to_host` to 0 and return what was there.
    pub fn take_to_host(&self) -> u64 {
        self.mask(TO_HOST_OFFSET).swap(0, Ordering::AcqRel)
    }

    /// The `bell-game` event, for the game's worker to wait on.
    pub fn bell_game(&self) -> HANDLE {
        self.bell_game.0
    }

    /// The `bell-host` event, for the helper's loop to wait on.
    pub fn bell_host(&self) -> HANDLE {
        self.bell_host.0
    }

    /// Take the state lock within `timeout_ms` (0 for game threads other than the worker). A lock
    /// whose owner died holding it counts as acquired, after the header is re-checked (design
    /// "Failures"). One guard at a time: while this thread holds one, `lock` returns `Busy`.
    pub fn lock(&self, timeout_ms: u32) -> Result<StateGuard<'_>, LockError> {
        // SAFETY: waits on our mutex handle.
        let r = unsafe { WaitForSingleObject(self.lock.0, timeout_ms) };
        match r {
            WAIT_OBJECT_0 => self.guard(),
            WAIT_ABANDONED => {
                let g = self.guard()?;
                match state::check_header(g.bytes()) {
                    Ok(()) => Ok(g),
                    Err(h) => Err(LockError::BadHeader(h)), // `g` drops here: released
                }
            }
            _ => Err(LockError::Busy),
        }
    }

    /// `lock` without the header check, for `create` (a new block has no header yet).
    fn lock_raw(&self, timeout_ms: u32) -> Option<StateGuard<'_>> {
        // SAFETY: waits on our mutex handle.
        let r = unsafe { WaitForSingleObject(self.lock.0, timeout_ms) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED { self.guard().ok() } else { None }
    }

    /// The mutex was just acquired: hand out the one guard, or undo a recursive acquire.
    fn guard(&self) -> Result<StateGuard<'_>, LockError> {
        if self.held.swap(true, Ordering::Acquire) {
            // This thread already holds a guard (only the owner can acquire a held mutex).
            // SAFETY: releases the extra, recursive acquisition made just now.
            unsafe { ReleaseMutex(self.lock.0) };
            return Err(LockError::Busy);
        }
        Ok(StateGuard { o: self, _not_send: PhantomData })
    }
}

/// The state block while its lock is held. Released on drop. Not `Send`: a mutex is owned by the
/// thread that took it, so the guard must be dropped there.
pub struct StateGuard<'a> {
    o: &'a Objects,
    _not_send: PhantomData<*const ()>,
}

impl StateGuard<'_> {
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the view holds at least BLOCK_SIZE bytes (checked when mapped) and the lock is
        // held, so no other thread or process writes it now.
        unsafe { std::slice::from_raw_parts(self.o.state.ptr, BLOCK_SIZE) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`; `&mut self` makes this the only borrow.
        unsafe { std::slice::from_raw_parts_mut(self.o.state.ptr, BLOCK_SIZE) }
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        // Clear the flag before the release, so the next owner finds it clear.
        self.o.held.store(false, Ordering::Release);
        // SAFETY: releases the mutex this thread acquired for this guard.
        unsafe { ReleaseMutex(self.o.lock.0) };
    }
}
