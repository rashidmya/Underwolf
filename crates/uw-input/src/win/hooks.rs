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
