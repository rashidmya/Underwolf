//! Native target discovery runs off the hook path; aliases must agree before installation.
use portable_atomic::AtomicU128;
use std::sync::atomic::Ordering;
use uw_pipe_proto::routing::Domains;
/// Coherent permission with a revocation generation, including revocation while already inactive.
pub struct OwnershipGate(AtomicU128);
impl Default for OwnershipGate {
    fn default() -> Self {
        Self::new()
    }
}
impl OwnershipGate {
    pub const fn new() -> Self {
        Self(AtomicU128::new(0))
    }
    pub fn ticket(&self) -> u128 {
        self.0.load(Ordering::Acquire)
    }
    pub fn publish(&self, ticket: u128, epoch: u64, domains: Domains, exclusive: bool) -> bool {
        let generation = ticket as u64 & !7;
        if epoch == 0
            || epoch < (ticket >> 64) as u64
            || generation == !7u64
            || (exclusive && !(domains.keyboard && domains.mouse))
        {
            return false;
        }
        let value = ((epoch as u128) << 64) | generation as u128 | domains.bits() as u128 | ((exclusive as u128) << 2);
        self.0.compare_exchange(ticket, value, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
    pub fn revoke(&self) {
        let _ = self.0.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            let generation = (value as u64 & !7).saturating_add(8) & !7;
            Some((value & !(u64::MAX as u128)) | generation as u128)
        });
    }
    pub fn snapshot(&self) -> (u64, Domains, bool) {
        let value = self.ticket();
        ((value >> 64) as u64, Domains { keyboard: value & 1 != 0, mouse: value & 2 != 0 }, value & 4 != 0)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    NullTarget,
    IncompatibleAlias,
    Capacity,
}

pub struct TargetRegistry {
    targets: Vec<(usize, SignatureId)>,
}
impl Default for TargetRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl TargetRegistry {
    pub fn new() -> Self {
        Self { targets: Vec::with_capacity(256) }
    }
    /// Discovery/installation only. A failed insertion leaves existing registrations intact.
    pub fn insert(&mut self, address: usize, signature: SignatureId) -> Result<bool, DiscoveryError> {
        if address == 0 {
            return Err(DiscoveryError::NullTarget);
        }
        if let Some((_, existing)) = self.targets.iter().find(|(target, _)| *target == address) {
            return if *existing == signature { Ok(false) } else { Err(DiscoveryError::IncompatibleAlias) };
        }
        if self.targets.len() == 256 {
            return Err(DiscoveryError::Capacity);
        }
        self.targets.push((address, signature));
        Ok(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeApi {
    DirectInput,
    GameInput,
}

/// Availability only: Discovered does not assert that isolation hooks are installed or tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    Absent,
    Discovered,
    Supported,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterCoverage {
    pub status: DiscoveryStatus,
    pub abi_mask: u16,
    pub detail: i32,
}
impl Default for AdapterCoverage {
    fn default() -> Self {
        Self { status: DiscoveryStatus::Absent, abi_mask: 0, detail: 0 }
    }
}
impl AdapterCoverage {
    pub fn failure(detail: i32) -> Self {
        let status = match detail as u32 {
            0x8000_4001 | 0x8000_4002 => DiscoveryStatus::Unsupported,
            _ => DiscoveryStatus::Failed,
        };
        Self { status, abi_mask: 0, detail }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryTarget {
    pub api: NativeApi,
    pub abi: u16,
    pub method: &'static str,
    pub address: usize,
    pub signature: SignatureId,
}

#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub directinput: AdapterCoverage,
    pub gameinput: AdapterCoverage,
    pub targets: Vec<DiscoveryTarget>,
    pub diagnostics: Vec<String>,
}
