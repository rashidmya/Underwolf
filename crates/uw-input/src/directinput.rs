//! Latest successful acquisition intent is independent of temporary native isolation.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeviceIntent {
    acquired: bool,
    isolated: bool,
    known: bool,
}
impl DeviceIntent {
    pub fn acquired(&mut self, original_hresult: i32) {
        if original_hresult >= 0 {
            self.acquired = true;
            self.known = true;
        }
    }
    pub fn unacquired(&mut self, original_hresult: i32) {
        if original_hresult >= 0 {
            self.acquired = false;
            self.known = true;
        }
    }
    /// DI_OK means this forced Unacquire found an acquired, possibly pre-existing device.
    pub fn isolated(&mut self, original_hresult: i32) {
        if original_hresult >= 0 {
            if original_hresult == 0 && !self.known {
                self.acquired = true;
            }
            self.isolated = true;
        }
    }
    pub fn is_isolated(&self) -> bool {
        self.isolated
    }
    pub fn restore_requested(&self) -> bool {
        self.acquired
    }
    pub fn may_restore(&self, physical_held: bool) -> bool {
        self.isolated && self.acquired && !physical_held
    }
    pub fn restored(&mut self, original_hresult: i32) {
        if original_hresult >= 0 {
            self.isolated = false;
        }
    }
}

use portable_atomic::AtomicU128;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceKind {
    Other = 0,
    Keyboard = 1,
    Mouse = 2,
}
impl DeviceKind {
    pub fn from_native_type(value: u32) -> Self {
        match value & 255 {
            0x12 => Self::Mouse,
            0x13 => Self::Keyboard,
            _ => Self::Other,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct DeviceToken {
    index: usize,
    stamp: u128,
}
impl DeviceToken {
    pub fn slot(self) -> usize {
        self.index
    }
    pub fn identity(self) -> usize {
        self.stamp as u64 as usize
    }
    pub fn kind(self) -> DeviceKind {
        match (self.stamp >> 66) & 3 {
            1 => DeviceKind::Keyboard,
            2 => DeviceKind::Mouse,
            _ => DeviceKind::Other,
        }
    }
}
#[derive(Debug)]
pub struct RegistryFull;
const INTENT: u128 = (3 << 64) | (1 << 68);
/// Scalar identity keys only; native borrowing requires its own lifetime gate or an explicitly owned reference.
/// Each slot publishes identity, generation, type and intent together; hook retries are bounded.
pub struct DeviceRegistry {
    entries: [AtomicU128; 256],
    next: AtomicU64,
    admission: AtomicBool,
}
impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl DeviceRegistry {
    pub fn token_at(&self, index: usize) -> Option<DeviceToken> {
        let value = self.entries.get(index)?.load(Ordering::Acquire);
        (value != 0).then_some(DeviceToken { index, stamp: value & !INTENT })
    }
    pub const fn new() -> Self {
        Self {
            entries: [const { AtomicU128::new(0) }; 256],
            next: AtomicU64::new(1),
            admission: AtomicBool::new(false),
        }
    }
    pub fn find(&self, identity: usize) -> Option<DeviceToken> {
        if identity == 0 {
            return None;
        }
        self.entries.iter().enumerate().find_map(|(index, slot)| {
            let value = slot.load(Ordering::Acquire);
            (value as u64 == identity as u64).then_some(DeviceToken { index, stamp: value & !INTENT })
        })
    }
    pub fn observe(&self, identity: usize, kind: DeviceKind) -> Result<DeviceToken, RegistryFull> {
        if identity == 0 {
            return Err(RegistryFull);
        }
        if let Some(token) = self.find(identity) {
            return if token.kind() == kind { Ok(token) } else { Err(RegistryFull) };
        }
        if self.admission.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(RegistryFull);
        }
        struct Admission<'a>(&'a AtomicBool);
        impl Drop for Admission<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _admission = Admission(&self.admission);
        if let Some(token) = self.find(identity) {
            return if token.kind() == kind { Ok(token) } else { Err(RegistryFull) };
        }
        let generation = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < (1 << 56) - 1).then_some(n + 1))
            .map_err(|_| RegistryFull)?;
        let stamp = identity as u128 | ((kind as u128) << 66) | ((generation as u128) << 72);
        for (index, slot) in self.entries.iter().enumerate() {
            if slot.compare_exchange(0, stamp, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Ok(DeviceToken { index, stamp });
            }
        }
        Err(RegistryFull)
    }
    pub fn created(&self, identity: usize, kind: DeviceKind) -> Result<DeviceToken, RegistryFull> {
        if let Some(old) = self.find(identity) {
            self.retire(old);
        }
        self.observe(identity, kind)
    }
    pub fn intent(&self, token: DeviceToken) -> Option<DeviceIntent> {
        let value = self.entries[token.index].load(Ordering::Acquire);
        (value & !INTENT == token.stamp).then_some(DeviceIntent {
            acquired: value & (1 << 64) != 0,
            isolated: value & (2 << 64) != 0,
            known: value & (1 << 68) != 0,
        })
    }
    pub fn update(&self, token: DeviceToken, intent: DeviceIntent) -> Result<(), RegistryFull> {
        let slot = &self.entries[token.index];
        let value = slot.load(Ordering::Acquire);
        if value & !INTENT != token.stamp {
            return Err(RegistryFull);
        }
        let next = token.stamp
            | ((intent.acquired as u128) << 64)
            | ((intent.isolated as u128) << 65)
            | ((intent.known as u128) << 68);
        slot.compare_exchange(value, next, Ordering::AcqRel, Ordering::Acquire).map(|_| ()).map_err(|_| RegistryFull)
    }
    pub fn apply(&self, token: DeviceToken, mut change: impl FnMut(&mut DeviceIntent)) -> Result<(), RegistryFull> {
        let slot = &self.entries[token.index];
        for _ in 0..4 {
            let value = slot.load(Ordering::Acquire);
            if value & !INTENT != token.stamp {
                return Err(RegistryFull);
            }
            let mut intent = DeviceIntent {
                acquired: value & (1 << 64) != 0,
                isolated: value & (2 << 64) != 0,
                known: value & (1 << 68) != 0,
            };
            change(&mut intent);
            let next = token.stamp
                | ((intent.acquired as u128) << 64)
                | ((intent.isolated as u128) << 65)
                | ((intent.known as u128) << 68);
            if slot.compare_exchange(value, next, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Ok(());
            }
        }
        Err(RegistryFull)
    }
    pub fn retire(&self, token: DeviceToken) {
        let slot = &self.entries[token.index];
        for _ in 0..4 {
            let value = slot.load(Ordering::Acquire);
            if value & !INTENT != token.stamp
                || slot.compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire).is_ok()
            {
                return;
            }
        }
    }
}
