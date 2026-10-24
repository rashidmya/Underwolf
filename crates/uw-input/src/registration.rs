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
    pub fn retire_target(&mut self, target: usize) {
        for entry in &mut self.entries {
            if target != 0 && entry.target == target {
                entry.target = 0;
                entry.flags = 1;
            }
        }
    }
    pub fn restore_descriptors(&self) -> Vec<Registration> {
        self.entries.clone()
    }
}

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
const OWNER: u64 = 1 << 63;
const CAPTURED: u64 = 1 << 62;
const GENERATION: u64 = 1 << 20;
const CALLS: u64 = GENERATION - 1;
/// Two coherent descriptors, with a nonblocking generation/count transaction gate.
/// Windows user handles have 32 significant bits, including on x64:
/// https://learn.microsoft.com/en-us/windows/win32/winprog64/interprocess-communication
/// Overlapping successful calls are ordered by each descriptor's atomic publication.
/// Owner writes converge after quiescence; overlapping native writes aren't atomic.
pub struct RegistrationJournal {
    state: AtomicU64,
    seen: AtomicU8,
    descriptors: [AtomicU64; 2],
    versions: [AtomicU64; 2],
}
impl Default for RegistrationJournal {
    fn default() -> Self {
        Self::new()
    }
}
impl RegistrationJournal {
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
            seen: AtomicU8::new(0),
            descriptors: [const { AtomicU64::new(0) }; 2],
            versions: [const { AtomicU64::new(0) }; 2],
        }
    }
    pub fn begin_call(&self) -> RegistrationCall<'_> {
        let before = self.state.fetch_add(GENERATION + 1, Ordering::AcqRel);
        RegistrationCall { journal: self, interfered: before & OWNER != 0, captured: before & CAPTURED != 0 }
    }
    pub fn begin_owner(&self) -> Option<u64> {
        let state = self.state.load(Ordering::Acquire);
        if state & (OWNER | CALLS) != 0 {
            return None;
        }
        self.state
            .compare_exchange(state, state | OWNER, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| state | OWNER)
    }
    pub fn commit_capture(&self, token: u64) -> bool {
        self.commit(token, true)
    }
    pub fn commit_owner(&self, token: u64) -> bool {
        self.commit(token, false)
    }
    fn commit(&self, token: u64, captured: bool) -> bool {
        if self
            .state
            .compare_exchange(
                token,
                (token & !(OWNER | CAPTURED)) | if captured { CAPTURED } else { 0 },
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            true
        } else {
            self.cancel_owner();
            false
        }
    }
    pub fn cancel_owner(&self) {
        self.state.fetch_and(!OWNER, Ordering::Release);
    }
    pub fn checkpoint(&self) -> [u64; 2] {
        self.versions.each_ref().map(|v| v.load(Ordering::Acquire))
    }
    pub fn current(&self, token: u64) -> bool {
        self.state.load(Ordering::Acquire) == token
    }
    pub fn merge(&self, ledger: &mut RegistrationLedger) {
        self.merge_since(ledger, [0; 2]);
    }
    pub fn merge_since(&self, ledger: &mut RegistrationLedger, baseline: [u64; 2]) {
        let seen = self.seen.load(Ordering::Acquire);
        for (index, usage) in [2, 6].into_iter().enumerate() {
            if seen & (1 << index) == 0 || self.versions[index].load(Ordering::Acquire) == baseline[index] {
                continue;
            }
            let value = self.descriptors[index].load(Ordering::Acquire);
            ledger.record_success(&[Registration {
                page: 1,
                usage,
                flags: value as u32,
                target: (value >> 32) as u32 as i32 as isize as usize,
            }]);
        }
    }
}
pub struct RegistrationCall<'a> {
    journal: &'a RegistrationJournal,
    interfered: bool,
    captured: bool,
}
impl RegistrationCall<'_> {
    pub fn captured(&self) -> bool {
        self.captured && !self.interfered
    }
    pub fn interfered(&self) -> bool {
        self.interfered
    }
    pub fn success(&self, r: Registration) {
        if !r.affected() {
            return;
        }
        let index = usize::from(r.usage == 6);
        self.journal.descriptors[index].store(((r.target as u32 as u64) << 32) | r.flags as u64, Ordering::Release);
        self.journal.seen.fetch_or(1 << index, Ordering::Release);
        self.journal.versions[index].fetch_add(1, Ordering::Release);
    }
}
impl Drop for RegistrationCall<'_> {
    fn drop(&mut self) {
        self.journal.state.fetch_sub(1, Ordering::Release);
    }
}
