//! Hook installation transaction. Published trampolines are never removed while callable.
#[derive(Debug, Clone, Copy)]
pub struct OwnedHook {
    pub target: usize,
    pub trampoline: usize,
}
#[derive(Debug)]
pub struct HookSet {
    pub hooks: Vec<OwnedHook>,
}
#[derive(Debug)]
pub struct InstallError {
    pub message: String,
    pub retained: Vec<OwnedHook>,
    pub rollback_errors: Vec<String>,
}
pub trait HookBackend {
    fn create(&mut self, target: usize, detour: usize) -> Result<usize, String>;
    fn publish(&mut self, target: usize, trampoline: usize);
    fn queue_enable(&mut self, target: usize) -> Result<(), String>;
    fn apply(&mut self) -> Result<(), String>;
    fn disable(&mut self, target: usize) -> Result<(), String>;
}
impl HookSet {
    pub fn install(backend: &mut impl HookBackend, targets: &[(usize, usize)]) -> Result<Self, InstallError> {
        let mut hooks = Vec::new();
        let work = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for &(target, detour) in targets {
                if target == 0 || detour == 0 || hooks.iter().any(|h: &OwnedHook| h.target == target) {
                    return Err("invalid or duplicate hook target".into());
                }
                let trampoline = backend.create(target, detour)?;
                hooks.push(OwnedHook { target, trampoline });
                backend.publish(target, trampoline);
            }
            for h in &hooks {
                backend.queue_enable(h.target)?
            }
            backend.apply()
        }));
        let failure = match work {
            Ok(Ok(())) => return Ok(Self { hooks }),
            Ok(Err(e)) => e,
            Err(_) => "hook backend panicked".into(),
        };
        let mut rollback_errors = Vec::new();
        for h in hooks.iter().rev() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.disable(h.target))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => rollback_errors.push(e),
                Err(_) => rollback_errors.push("hook rollback panicked".into()),
            }
        }
        Err(InstallError { message: failure, retained: hooks, rollback_errors })
    }
}
