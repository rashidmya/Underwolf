//! MinHook backend. Only targets owned by this module are ever disabled.
use crate::hooks::HookBackend;
use minhook::MinHook;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    fn native_partial_create_failure_retains_callable_original() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
        let target = GetKeyState as *const () as usize;
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
