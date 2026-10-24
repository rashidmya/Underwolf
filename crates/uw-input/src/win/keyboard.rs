//! Game-facing state queries. Internal calls use published originals and a recursion bypass.
use super::*;
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    UI::Input::KeyboardAndMouse::*,
};
static GET_KEY: AtomicUsize = AtomicUsize::new(0);
static GET_ASYNC: AtomicUsize = AtomicUsize::new(0);
static GET_KEYS: AtomicUsize = AtomicUsize::new(0);
pub(super) fn targets() -> Vec<hooks::Target> {
    vec![
        hooks::Target {
            address: GetKeyState as *const () as usize,
            detour: key_state as *const () as usize,
            original: &GET_KEY,
        },
        hooks::Target {
            address: GetAsyncKeyState as *const () as usize,
            detour: async_state as *const () as usize,
            original: &GET_ASYNC,
        },
        hooks::Target {
            address: GetKeyboardState as *const () as usize,
            detour: keyboard_state as *const () as usize,
            original: &GET_KEYS,
        },
    ]
}
fn hidden(vk: i32) -> bool {
    if bypassed() {
        return false;
    }
    SHARED.get().is_some_and(|s| s.blocks_key(vk))
}
pub(super) fn real_async(vk: i32) -> i16 {
    let p = GET_ASYNC.load(Ordering::Acquire);
    // SAFETY: original trampoline has this API's signature; before installation call the actual API.
    unsafe {
        if p == 0 {
            GetAsyncKeyState(vk)
        } else {
            std::mem::transmute::<usize, unsafe extern "system" fn(i32) -> i16>(p)(vk)
        }
    }
}
unsafe extern "system" fn key_state(vk: i32) -> i16 {
    boundary(0, || {
        // SAFETY: original was published before enabling this ABI-matching hook.
        let original = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(i32) -> i16>(GET_KEY.load(Ordering::Acquire))
        };
        // SAFETY: forwards caller argument unchanged.
        let value = unsafe { original(vk) };
        if hidden(vk) { value & 1 } else { value }
    })
}
unsafe extern "system" fn async_state(vk: i32) -> i16 {
    boundary(0, || {
        let value = real_async(vk);
        if hidden(vk) { 0 } else { value }
    })
}
unsafe extern "system" fn keyboard_state(out: *mut u8) -> i32 {
    boundary(0, || {
        // SAFETY: original was published before enabling this hook.
        let original = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut u8) -> i32>(GET_KEYS.load(Ordering::Acquire))
        };
        // SAFETY: caller supplies the API buffer; original validates/writes it.
        let result = unsafe { original(out) };
        // SAFETY: GetLastError has no pointer preconditions.
        let error = unsafe { GetLastError() };
        if result != 0 && !out.is_null() && !bypassed() {
            for vk in 0..256 {
                if hidden(vk) {
                    // SAFETY: successful GetKeyboardState wrote all 256 bytes.
                    unsafe {
                        *out.add(vk as usize) &= 1;
                    }
                }
            }
        }
        // SAFETY: restore original call's error state.
        unsafe { SetLastError(error) };
        result
    })
}
