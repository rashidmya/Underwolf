//! Native target discovery runs off the hook path; aliases must agree before installation.
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
