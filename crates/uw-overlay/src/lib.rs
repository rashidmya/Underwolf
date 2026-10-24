//! Production overlay entry validation; the injected DLL owns input for the game session.
use std::sync::atomic::{AtomicBool, Ordering};
#[repr(C)]
pub struct StartParams {
    pub size: u32,
    pub version: u32,
    pub pid: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    Short,
    Version,
    Pid,
}
pub fn validate_params(size: u32, version: u32, pid: u32, current_pid: u32) -> Result<(), StartError> {
    if size < 12 {
        Err(StartError::Short)
    } else if version != 1 {
        Err(StartError::Version)
    } else if pid != current_pid {
        Err(StartError::Pid)
    } else {
        Ok(())
    }
}
#[derive(Default)]
pub struct StartOnce(AtomicBool);
impl StartOnce {
    pub fn claim(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
}
#[cfg(windows)]
mod win;
