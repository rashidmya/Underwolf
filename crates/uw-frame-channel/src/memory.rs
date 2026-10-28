//! Owned virtual reservations keep host imports aligned without exposing mutable slices.
use crate::Result;
use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, memfd_create};
use std::{
    fs::File,
    os::fd::AsRawFd,
    sync::atomic::{AtomicUsize, Ordering},
};
use uw_frame_proto::FrameError;
static LIVE: AtomicUsize = AtomicUsize::new(0);
const SEALS: SealFlags = SealFlags::GROW
    .union(SealFlags::SHRINK)
    .union(SealFlags::SEAL);
pub(crate) fn new_file(name: &str, len: u64) -> Result<File> {
    let file = File::from(
        memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
            .map_err(|_| FrameError::Io)?,
    );
    file.set_len(len).map_err(|_| FrameError::Io)?;
    fcntl_add_seals(&file, SEALS).map_err(|_| FrameError::Io)?;
    Ok(file)
}
pub(crate) fn check_file(file: &File, len: u64, uid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|_| FrameError::Io)?;
    if !m.is_file()
        || m.len() != len
        || m.uid() != uid
        || !fcntl_get_seals(file)
            .map_err(|_| FrameError::Invalid)?
            .contains(SEALS)
    {
        return Err(FrameError::Invalid);
    }
    Ok(())
}
pub(crate) struct Memory {
    base: usize,
    reserved: usize,
    pointer: usize,
    len: usize,
}
// SAFETY: ownership of the mapped range travels with this value; byte access is
// performed only by the locked frame protocol or by explicitly unsafe GPU APIs.
unsafe impl Send for Memory {}
// SAFETY: no safe mutable slice is exposed; shared byte accesses follow the
// process-shared mutex and immutable-until-fence-completion protocol.
unsafe impl Sync for Memory {}
impl Memory {
    pub(crate) fn new(file: &File, len: usize, alignment: usize) -> Result<Self> {
        let reserved = len.checked_add(alignment).ok_or(FrameError::Overflow)?;
        // SAFETY: reserves a new anonymous range, with no existing address supplied.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                reserved,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(FrameError::Io);
        }
        LIVE.fetch_add(1, Ordering::Relaxed);
        let mut memory = Self {
            base: base as usize,
            reserved,
            pointer: base as usize,
            len,
        };
        let pointer = memory
            .base
            .checked_add(alignment - 1)
            .ok_or(FrameError::Overflow)?
            & !(alignment - 1);
        // SAFETY: MAP_FIXED replaces only a subrange of our still-owned reservation.
        // Both file length and page/import alignment were validated before this call.
        let mapped = unsafe {
            libc::mmap(
                pointer as *mut _,
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_FIXED,
                file.as_raw_fd(),
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(FrameError::Io);
        }
        memory.pointer = pointer;
        Ok(memory)
    }
    pub(crate) fn pointer(&self) -> *mut u8 {
        self.pointer as *mut u8
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: the entire reservation remains ours, including its PROT_NONE
        // padding. Frame/GPU owners retain this Memory until no accesses remain.
        unsafe { libc::munmap(self.base as *mut _, self.reserved) };
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}
pub fn live_mapping_count() -> usize {
    LIVE.load(Ordering::Relaxed)
}
