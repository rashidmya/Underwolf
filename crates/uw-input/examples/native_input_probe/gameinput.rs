//! Independent GameInput factory check; no adapter ABI declarations are imported.
use std::ffi::c_void;
use windows_sys::{
    Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    core::w,
};

pub fn factory_result() -> Option<i32> {
    // SAFETY: checks only this isolated probe's loaded module.
    let module = unsafe { GetModuleHandleW(w!("GameInput.dll")) };
    if module.is_null() {
        return None;
    }
    // SAFETY: the export name is the pinned SDK's C factory, not the inline versioned wrapper.
    let address = unsafe { GetProcAddress(module, c"GameInputCreate".as_ptr().cast()) }.unwrap();
    type Create = unsafe extern "system" fn(*mut *mut c_void) -> i32;
    // SAFETY: independently declared GameInputCreate signature returns the legacy IUnknown-compatible interface.
    let create = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(address) };
    let mut output = std::ptr::null_mut();
    // SAFETY: output points to a writable local reference slot.
    let result = unsafe { create(&mut output) };
    if result < 0 {
        assert!(output.is_null(), "failed native factory retained an unexpected output");
    } else {
        assert!(!output.is_null());
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;
        // SAFETY: successful SDK factory returns an owned IUnknown-compatible object with Release at slot 2.
        let release = unsafe { std::mem::transmute::<usize, Release>(*(*(output.cast::<*const usize>())).add(2)) };
        // SAFETY: release exactly the independent probe's one acquired reference.
        unsafe { release(output) };
    }
    Some(result)
}
