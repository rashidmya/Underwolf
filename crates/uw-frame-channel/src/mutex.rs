//! Native mutex bytes are trusted only after descriptor, ABI and peer validation.
use crate::Result;
use std::{
    marker::PhantomData,
    mem::{MaybeUninit, align_of, size_of},
};
use uw_frame_proto::FrameError;
pub(crate) fn check_abi() -> Result<()> {
    // SAFETY: sysconf only queries the current process's page-size configuration.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if !cfg!(all(target_arch = "x86_64", target_env = "gnu"))
        || page != 4096
        || size_of::<libc::pthread_mutex_t>() != 40
        || align_of::<libc::pthread_mutex_t>() != 8
    {
        return Err(FrameError::Unsupported);
    }
    Ok(())
}
pub(crate) unsafe fn initialize(pointer: *mut libc::pthread_mutex_t) -> Result<()> {
    check_abi()?;
    let mut attr = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
    // SAFETY: attr has valid writable storage of the native attribute type.
    if unsafe { libc::pthread_mutexattr_init(attr.as_mut_ptr()) } != 0 {
        return Err(FrameError::Unsupported);
    }
    // SAFETY: pthread_mutexattr_init succeeded and initialized the attribute.
    let mut attr = unsafe { attr.assume_init() };
    // SAFETY: attr is initialized, and pointer is fresh, aligned writable storage
    // owned by the caller, not yet shared with another thread or process.
    let rc = unsafe {
        let shared = libc::pthread_mutexattr_setpshared(&mut attr, libc::PTHREAD_PROCESS_SHARED);
        let robust = libc::pthread_mutexattr_setrobust(&mut attr, libc::PTHREAD_MUTEX_ROBUST);
        if shared != 0 {
            shared
        } else if robust != 0 {
            robust
        } else {
            libc::pthread_mutex_init(pointer, &attr)
        }
    };
    // SAFETY: the attribute is initialized and no longer needed after init.
    unsafe { libc::pthread_mutexattr_destroy(&mut attr) };
    if rc == 0 {
        Ok(())
    } else {
        Err(FrameError::Unsupported)
    }
}
pub(crate) struct Guard {
    pointer: *mut libc::pthread_mutex_t,
    _not_send: PhantomData<*mut ()>,
}
pub(crate) enum Lock {
    Acquired(Guard),
    Busy,
    OwnerDead(Guard),
    Poisoned,
}
pub(crate) unsafe fn try_lock(pointer: *mut libc::pthread_mutex_t) -> Result<Lock> {
    // SAFETY: the caller retains the mapping and verified native initialization
    // and ownership of this mutex; this call never waits for its current owner.
    let rc = unsafe { libc::pthread_mutex_trylock(pointer) };
    match rc {
        0 => Ok(Lock::Acquired(Guard {
            pointer,
            _not_send: PhantomData,
        })),
        libc::EBUSY => Ok(Lock::Busy),
        libc::EOWNERDEAD => Ok(Lock::OwnerDead(Guard {
            pointer,
            _not_send: PhantomData,
        })),
        libc::ENOTRECOVERABLE => Ok(Lock::Poisoned),
        _ => Err(FrameError::Invalid),
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: only an owning thread can construct/drop this non-Send guard.
        // For EOWNERDEAD, deliberately do not make the damaged mutex consistent;
        // unlocking poisons this abandoned allocation for all future callers.
        unsafe { libc::pthread_mutex_unlock(self.pointer) };
    }
}
