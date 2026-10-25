//! MinHook backend. Only targets owned by this module are ever disabled.
use crate::hooks::HookBackend;
use minhook::MinHook;
use std::sync::atomic::{AtomicUsize, Ordering};
/// GNU function pointers can name this DLL's private import jump stub. Resolve the
/// shared export so calls through other modules' imports and GetProcAddress reach us too.
/// Zero is deliberately rejected by the hook installation transaction.
pub(super) fn resolve(name: &std::ffi::CStr) -> usize {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::core::w;
    // SAFETY: user32 is a static dependency and already in use by the owner-thread installer.
    let module = unsafe { GetModuleHandleW(w!("user32.dll")) };
    if module.is_null() {
        return 0;
    }
    // SAFETY: loaded system module and a borrowed NUL-terminated export name.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }.map_or(0, |function| function as usize)
}
pub struct Target {
    pub address: usize,
    pub detour: usize,
    pub original: &'static AtomicUsize,
}
pub struct Backend<'a>(pub &'a [Target]);
impl HookBackend for Backend<'_> {
    fn create(&mut self, target: usize, detour: usize) -> Result<usize, String> {
        // SAFETY: the installer resolves executable API addresses and ABI-matching detours.
        unsafe { MinHook::create_hook(target as *mut _, detour as *mut _) }
            .map(|p| p as usize)
            .map_err(|e| format!("create {target:x}: {e:?}"))
    }
    fn publish(&mut self, target: usize, trampoline: usize) {
        if let Some(t) = self.0.iter().find(|t| t.address == target) {
            t.original.store(trampoline, Ordering::Release)
        }
    }
    fn queue_enable(&mut self, target: usize) -> Result<(), String> {
        // SAFETY: this target was created and its trampoline published above.
        unsafe { MinHook::queue_enable_hook(target as *mut _) }.map_err(|e| format!("queue enable: {e:?}"))
    }
    fn apply(&mut self) -> Result<(), String> {
        // SAFETY: queued targets have valid, resident callbacks and published originals.
        unsafe { MinHook::apply_queued() }.map_err(|e| format!("apply: {e:?}"))
    }
    fn disable(&mut self, target: usize) -> Result<(), String> {
        // SAFETY: cancel any queued enable for this owned target before direct disable.
        unsafe { MinHook::queue_disable_hook(target as *mut _) }.map_err(|e| format!("cancel enable: {e:?}"))?;
        // SAFETY: only owned targets are passed; their trampolines are retained.
        unsafe { MinHook::disable_hook(target as *mut _) }.map_err(|e| format!("disable: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn detour(vk: i32) -> i16 {
        // SAFETY: published native GetKeyState trampoline has this exact ABI.
        unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(i32) -> i16>(ORIGINAL.load(Ordering::Acquire))(vk)
        }
    }
    #[test]
    fn native_adapters_target_shared_exports_instead_of_private_import_stubs() {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows_sys::core::w;
        let groups = [
            (super::super::keyboard::targets(), vec![c"GetKeyState", c"GetAsyncKeyState", c"GetKeyboardState"]),
            (super::super::raw::targets(), vec![c"GetRawInputData", c"GetRawInputBuffer"]),
            (super::super::registration::targets(), vec![c"RegisterRawInputDevices"]),
            (
                super::super::cursor::targets(),
                vec![
                    c"GetCursorPos",
                    c"SetCursorPos",
                    c"ClipCursor",
                    c"GetClipCursor",
                    c"ShowCursor",
                    c"SetCursor",
                    c"GetCursor",
                ],
            ),
        ];
        // SAFETY: user32 is already loaded by these statically imported APIs.
        let module = unsafe { GetModuleHandleW(w!("user32.dll")) };
        assert!(!module.is_null());
        for (targets, names) in groups {
            assert_eq!(targets.len(), names.len());
            for (target, name) in targets.into_iter().zip(names) {
                // SAFETY: loaded module and static NUL-terminated export name.
                let export = unsafe { GetProcAddress(module, name.as_ptr().cast()) }.expect("required system export");
                assert_eq!(target.address, export as usize, "{name:?} hook must intercept other modules too");
            }
        }
    }
    #[test]
    fn missing_system_export_is_rejected_before_hook_creation() {
        let target = resolve(c"UnderwolfMissingSystemExport");
        assert_eq!(target, 0);
        let targets = [Target { address: target, detour: detour as *const () as usize, original: &ORIGINAL }];
        assert!(crate::hooks::HookSet::install(&mut Backend(&targets), &[(target, targets[0].detour)]).is_err());
    }
    #[test]
    fn native_partial_create_failure_retains_callable_original() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
        let target = resolve(c"GetKeyState");
        let targets = [Target { address: target, detour: detour as *const () as usize, original: &ORIGINAL }];
        let result = crate::hooks::HookSet::install(
            &mut Backend(&targets),
            &[(target, targets[0].detour), (1, targets[0].detour)],
        );
        let failure = result.expect_err("invalid second native target must fail");
        assert_eq!(failure.retained.len(), 1);
        assert_ne!(ORIGINAL.load(Ordering::Acquire), 0);
        // SAFETY: scalar native query and ABI-correct retained trampoline remain callable after rollback.
        unsafe {
            assert_eq!(detour(0), GetKeyState(0));
        }
    }
}
