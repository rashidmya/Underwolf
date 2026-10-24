//! The game's latest successful registration intent, independent of our temporary transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    pub page: u16,
    pub usage: u16,
    pub flags: u32,
    pub target: usize,
}
impl Registration {
    pub fn affected(&self) -> bool {
        self.page == 1 && matches!(self.usage, 2 | 6)
    }
    pub fn captured(mut self) -> Self {
        if self.affected() && self.flags & 1 == 0 {
            self.flags &= !0x30;
            if self.usage == 2 { self.flags &= !0x200 } else { self.flags &= !0x400 }
        }
        self
    }
}
pub struct RegistrationLedger {
    entries: Vec<Registration>,
}
impl RegistrationLedger {
    pub fn new(initial: &[Registration]) -> Self {
        Self { entries: initial.iter().copied().filter(Registration::affected).collect() }
    }
    pub fn record_success(&mut self, requests: &[Registration]) {
        for &r in requests {
            if !r.affected() {
                continue;
            }
            if let Some(old) = self.entries.iter_mut().find(|old| old.page == r.page && old.usage == r.usage) {
                *old = r
            } else {
                self.entries.push(r)
            }
        }
    }
    pub fn capture_descriptors(&self) -> Vec<Registration> {
        self.entries.iter().copied().map(Registration::captured).collect()
    }
    pub fn restore_descriptors(&self) -> Vec<Registration> {
        self.entries.clone()
    }
}
