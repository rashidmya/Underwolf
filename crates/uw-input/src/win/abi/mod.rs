//! Borrowed system interfaces only; API versions are selected by their actual QueryInterface IID.
pub mod directinput;
pub mod gameinput;
use std::{ffi::c_void, ptr::NonNull};
use windows_sys::core::GUID;
pub type Raw = *mut c_void;
pub type QueryInterface = unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32;
pub type Reference = unsafe extern "system" fn(Raw) -> u32;

/// Owns exactly one returned system COM reference, without wrapping borrowed game interfaces.
pub struct OwnedCom {
    raw: NonNull<c_void>,
    release: Reference,
}
impl OwnedCom {
    /// # Safety
    /// `raw` is null or an owned IUnknown-compatible reference returned by a successful system call.
    pub unsafe fn from_owned(raw: Raw) -> Option<Self> {
        let raw = NonNull::new(raw)?;
        // SAFETY: the successful native factory/QI supplies an IUnknown vtable with Release at slot 2.
        let release =
            unsafe { std::mem::transmute::<usize, Reference>(*(*(raw.as_ptr().cast::<*const usize>())).add(2)) };
        Some(Self { raw, release })
    }
    pub fn raw(&self) -> Raw {
        self.raw.as_ptr()
    }
    /// # Safety
    /// `slot` is in the vtable of the interface IID used to obtain this reference.
    pub unsafe fn slot(&self, slot: usize) -> usize {
        // SAFETY: caller establishes the IID-specific vtable bound; the reference remains owned.
        unsafe { *(*(self.raw().cast::<*const usize>())).add(slot) }
    }
    pub fn query(&self, iid: &GUID) -> Result<Self, i32> {
        // SAFETY: every owned native interface has the IUnknown prefix.
        let query = unsafe { std::mem::transmute::<usize, QueryInterface>(self.slot(0)) };
        let mut output = std::ptr::null_mut();
        // SAFETY: valid owned interface, borrowed IID, and writable output local.
        let result = unsafe { query(self.raw(), iid, &mut output) };
        if result < 0 {
            return Err(result);
        }
        // SAFETY: successful QI returns exactly one owned reference.
        unsafe { Self::from_owned(output) }.ok_or(0x8000_4003u32 as i32)
    }
}
impl Drop for OwnedCom {
    fn drop(&mut self) {
        // SAFETY: this owner releases precisely the reference it acquired, using its original method.
        unsafe { (self.release)(self.raw()) };
    }
}
