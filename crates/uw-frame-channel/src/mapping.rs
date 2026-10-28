use crate::{
    Budget, BudgetReservation, PeerIdentity, Result,
    memory::{self, Memory},
    mutex,
};
use std::{
    fs::File,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::fs::FileExt,
    },
    sync::Arc,
};
use uw_frame_proto::*;

/// The only mutable regions are the native mutex and state tail, protected by
/// that mutex, plus pixels which stay immutable during a claimed GPU read.
pub struct FrameMapping {
    header: AllocationHeader,
    control: Memory,
    pixels: Memory,
    files: [File; 2],
    _reservation: BudgetReservation,
}
pub struct ReadLease {
    mapping: Arc<FrameMapping>,
    claim: Claim,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    Published { seq: u64, bytes: u64 },
    Busy,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteResult {
    Completed,
    Busy,
    Stale,
}
pub struct Patch<'a> {
    pub rect: Rect,
    pub stride: u32,
    pub data: &'a [u8],
}

impl FrameMapping {
    /// Standalone allocation; sessions must use create_in with their shared budget.
    pub fn create(key: AllocationKey, layout: FrameLayout, peer: PeerIdentity) -> Result<Self> {
        Self::create_in(&Budget::new(SHARED_BUDGET), key, layout, peer)
    }
    pub fn create_in(
        budget: &Budget,
        key: AllocationKey,
        layout: FrameLayout,
        peer: PeerIdentity,
    ) -> Result<Self> {
        mutex::check_abi()?;
        let current = PeerIdentity::current()?;
        if peer != PeerIdentity::for_pid(peer.pid)? || peer.uid != current.uid {
            return Err(FrameError::Stale);
        }
        let header = AllocationHeader {
            key,
            consumer_pid: peer.pid,
            producer_pid: current.pid,
            start_ticks: peer.start_ticks,
            layout,
        };
        let encoded = header.encode()?;
        let reservation = budget.reserve(layout.pixel_len)?;
        let files = [
            memory::new_file("uw-frame-control", CONTROL_BYTES as u64)?,
            memory::new_file("uw-frame-pixels", layout.pixel_len)?,
        ];
        let control = Memory::new(&files[0], CONTROL_BYTES, 4096)?;
        let pixels = Memory::new(
            &files[1],
            layout.pixel_len as usize,
            layout.alignment as usize,
        )?;
        // SAFETY: the new sealed control mapping is exclusively owned, large
        // enough, and no descriptor or pointer has been exposed to another user.
        unsafe {
            std::ptr::copy_nonoverlapping(encoded.as_ptr(), control.pointer(), encoded.len());
        }
        // SAFETY: byte 128 is aligned native mutex storage in a fresh mapping.
        unsafe {
            mutex::initialize(control.pointer().add(128).cast())?;
        }
        let mapping = Self {
            header,
            control,
            pixels,
            files,
            _reservation: reservation,
        };
        mapping.write_state(&FrameState::new(key, layout));
        Ok(mapping)
    }
    /// # Safety
    /// A trusted producer must have initialized the native mutex exactly once.
    /// All holders of writable descriptors must preserve immutable metadata and
    /// obey this crate's mutex/publication/GPU ownership protocol for their lifetime.
    pub unsafe fn open(header: &[u8], fds: [OwnedFd; 2], peer: PeerIdentity) -> Result<Self> {
        // SAFETY: forwards the caller's native initialization/ownership contract.
        unsafe { Self::open_in(&Budget::new(SHARED_BUDGET), header, fds, peer) }
    }
    /// # Safety
    /// Same native initialization and ownership contract as open. The session
    /// must retain its budget reservation while retired GPU users still exist.
    pub unsafe fn open_in(
        budget: &Budget,
        encoded: &[u8],
        fds: [OwnedFd; 2],
        peer: PeerIdentity,
    ) -> Result<Self> {
        mutex::check_abi()?;
        let header = AllocationHeader::decode(encoded)?;
        let current = PeerIdentity::current()?;
        if peer != PeerIdentity::for_pid(peer.pid)?
            || peer.uid != current.uid
            || header.consumer_pid != current.pid
            || header.start_ticks != current.start_ticks
            || header.producer_pid != peer.pid
        {
            return Err(FrameError::Stale);
        }
        let [control_file, pixel_file] = fds;
        let files = [File::from(control_file), File::from(pixel_file)];
        memory::check_file(&files[0], CONTROL_BYTES as u64, peer.uid)?;
        memory::check_file(&files[1], header.layout.pixel_len, peer.uid)?;
        let mut immutable = [0; 128];
        files[0]
            .read_exact_at(&mut immutable, 0)
            .map_err(|_| FrameError::Io)?;
        if immutable.as_slice() != encoded {
            return Err(FrameError::Invalid);
        }
        // The native mutex/state may be changing; inspect only immutable padding.
        for (start, len) in [(168, 24), (256, CONTROL_BYTES - 256)] {
            let mut padding = vec![0; len];
            files[0]
                .read_exact_at(&mut padding, start as u64)
                .map_err(|_| FrameError::Io)?;
            if padding.iter().any(|&b| b != 0) {
                return Err(FrameError::Invalid);
            }
        }
        let reservation = budget.reserve(header.layout.pixel_len)?;
        let control = Memory::new(&files[0], CONTROL_BYTES, 4096)?;
        let pixels = Memory::new(
            &files[1],
            header.layout.pixel_len as usize,
            header.layout.alignment as usize,
        )?;
        Ok(Self {
            header,
            control,
            pixels,
            files,
            _reservation: reservation,
        })
    }
    /// # Safety
    /// Descriptors are writable. Receivers must preserve immutable headers,
    /// native initialization and the locked ownership protocol. They may not
    /// use file writes to bypass synchronization or initialize/reset the mutex.
    pub unsafe fn descriptors(&self) -> [BorrowedFd<'_>; 2] {
        [self.files[0].as_fd(), self.files[1].as_fd()]
    }
    pub fn header(&self) -> AllocationHeader {
        self.header
    }
    fn read_state(&self) -> Result<FrameState> {
        let mut bytes = [0; STATE_BYTES];
        // SAFETY: callers hold the process-shared mutex; state lies completely
        // inside this retained mapping. No Rust reference aliases mutable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.control.pointer().add(STATE_OFFSET),
                bytes.as_mut_ptr(),
                STATE_BYTES,
            );
        }
        FrameState::decode(self.header.key, self.header.layout, &bytes)
    }
    fn write_state(&self, state: &FrameState) {
        let bytes = state.encode();
        // SAFETY: either this mapping is not yet shared or the caller owns its
        // native mutex. The encoded tail has the exact validated fixed size.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.control.pointer().add(STATE_OFFSET),
                STATE_BYTES,
            );
        }
    }
    fn invalidate_locked(&self) {
        let mut s = FrameState::new(self.header.key, self.header.layout);
        s.invalidate();
        self.write_state(&s);
    }
    fn lock(&self) -> Result<FrameGuard<'_>> {
        // SAFETY: constructors establish native ABI, initialization and lifetime;
        // exported/opened descriptors carry that same explicit safety contract.
        let lock = unsafe { mutex::try_lock(self.control.pointer().add(128).cast())? };
        match lock {
            mutex::Lock::Acquired(lock) => Ok(FrameGuard {
                mapping: self,
                _lock: lock,
                dirty: false,
            }),
            mutex::Lock::Busy => Err(FrameError::Busy),
            mutex::Lock::OwnerDead(lock) => {
                self.invalidate_locked();
                drop(lock);
                Err(FrameError::OwnerDied)
            }
            mutex::Lock::Poisoned => Err(FrameError::OwnerDied),
        }
    }
    pub fn try_state(&self) -> Result<StateSnapshot> {
        let guard = self.lock()?;
        Ok(guard.state()?.snapshot())
    }
    pub fn try_publish(&self, seq: u64, patch: Patch<'_>) -> Result<PublishResult> {
        if std::process::id() != self.header.producer_pid {
            return Err(FrameError::Invalid);
        }
        let bounds = (self.header.layout.width, self.header.layout.height);
        let rect = Rect::checked(
            patch.rect.x,
            patch.rect.y,
            patch.rect.width,
            patch.rect.height,
            bounds,
        )?
        .ok_or(FrameError::Invalid)?;
        if patch.stride < rect.width * 4
            || patch.stride > MAX_SIDE * 4
            || !patch.stride.is_multiple_of(4)
            || patch.data.len() as u64 != u64::from(patch.stride) * u64::from(rect.height)
        {
            return Err(FrameError::Invalid);
        }
        let mut guard = match self.lock() {
            Ok(g) => g,
            Err(FrameError::Busy) => return Ok(PublishResult::Busy),
            Err(e) => return Err(e),
        };
        let mut state = guard.state()?;
        match state.publish(seq, rect) {
            Ok(()) => {}
            Err(FrameError::Busy) => return Ok(PublishResult::Busy),
            Err(FrameError::Stale) => return Ok(PublishResult::Stale),
            Err(e) => return Err(e),
        }
        guard.dirty = true;
        for row in 0..rect.height as usize {
            let source = row * patch.stride as usize;
            let destination =
                (rect.y as usize + row) * self.header.layout.stride as usize + rect.x as usize * 4;
            // SAFETY: all source/destination extents were validated before the
            // first write. No GPU can own pixels while this publication commits.
            // Safe callers cannot obtain an aliasing slice of mapped pixel data.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    patch.data.as_ptr().add(source),
                    self.pixels.pointer().add(destination),
                    rect.width as usize * 4,
                );
            }
        }
        guard.store(&state);
        guard.dirty = false;
        Ok(PublishResult::Published {
            seq,
            bytes: u64::from(rect.width) * u64::from(rect.height) * 4,
        })
    }
    pub fn try_claim(self: &Arc<Self>) -> Result<Option<ReadLease>> {
        if std::process::id() != self.header.consumer_pid {
            return Err(FrameError::Invalid);
        }
        let guard = self.lock()?;
        let mut state = guard.state()?;
        let claim = state.claim()?;
        guard.store(&state);
        Ok(claim.map(|claim| ReadLease {
            mapping: self.clone(),
            claim,
        }))
    }
    fn finish(&self, lease: &ReadLease, complete: bool) -> Result<CompleteResult> {
        if !std::ptr::eq(self, lease.mapping.as_ref()) {
            return Ok(CompleteResult::Stale);
        }
        let guard = match self.lock() {
            Ok(g) => g,
            Err(FrameError::Busy) => return Ok(CompleteResult::Busy),
            Err(e) => return Err(e),
        };
        let mut state = guard.state()?;
        let result = if complete {
            state.complete(lease.claim)
        } else {
            state.cancel_unsubmitted(lease.claim)
        };
        match result {
            Ok(()) => {
                guard.store(&state);
                Ok(CompleteResult::Completed)
            }
            Err(FrameError::Stale) => Ok(CompleteResult::Stale),
            Err(e) => Err(e),
        }
    }
    pub fn try_cancel_unsubmitted(&self, lease: &ReadLease) -> Result<CompleteResult> {
        self.finish(lease, false)
    }
    /// # Safety
    /// Every GPU read using this lease must have completed, as proved by its
    /// actual submission fence (not a timeout, disconnect or queued receipt).
    pub unsafe fn try_complete_after_gpu(&self, lease: &ReadLease) -> Result<CompleteResult> {
        self.finish(lease, true)
    }
}
struct FrameGuard<'a> {
    mapping: &'a FrameMapping,
    _lock: mutex::Guard,
    dirty: bool,
}
impl FrameGuard<'_> {
    fn state(&self) -> Result<FrameState> {
        self.mapping.read_state()
    }
    fn store(&self, state: &FrameState) {
        self.mapping.write_state(state);
    }
}
impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        if self.dirty || std::thread::panicking() {
            self.mapping.invalidate_locked();
        }
    }
}
impl ReadLease {
    pub fn claim(&self) -> Claim {
        self.claim
    }
    pub fn mapping(&self) -> &Arc<FrameMapping> {
        &self.mapping
    }
    /// The pointer is valid while this lease/mapping is retained. Dereferencing
    /// or importing it is unsafe and must preserve the claimed GPU lifetime.
    pub fn host_pointer(&self) -> *mut std::ffi::c_void {
        self.mapping.pixels.pointer().cast()
    }
    pub fn copy_pixels(&self) -> Result<Vec<u8>> {
        let guard = self.mapping.lock()?;
        let state = guard.state()?.snapshot();
        if state.phase != Phase::Reading
            || state.claimed != self.claim.seq
            || state.damage != Some(self.claim.damage)
        {
            return Err(FrameError::Stale);
        }
        let mut bytes = vec![0; self.mapping.pixels.len()];
        // SAFETY: the retained, validated mapping stays alive and the mutex and
        // Reading state exclude any producer write while these bytes are copied.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.mapping.pixels.pointer(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
        }
        Ok(bytes)
    }
}
pub fn live_mapping_count() -> usize {
    memory::live_mapping_count()
}
