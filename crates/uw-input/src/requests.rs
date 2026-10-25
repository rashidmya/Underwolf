//! Bounded cross-thread requests. Enqueue is not application; the UI dispatches later.
use crate::configuration::PreparedConfig;
use crate::policy::{Effects, Policy, PolicyEvent};
use std::collections::VecDeque;
use std::sync::Mutex;
use uw_pipe_proto::input::CaptureRequest;
#[derive(Debug, Clone, Copy)]
pub struct Full;
pub struct RequestQueue(Mutex<VecDeque<CaptureRequest>>);
impl Default for RequestQueue {
    fn default() -> Self {
        Self::new()
    }
}
impl RequestQueue {
    pub fn new() -> Self {
        Self(Mutex::new(VecDeque::with_capacity(32)))
    }
    pub fn try_push(&self, r: CaptureRequest) -> Result<(), Full> {
        let Ok(mut q) = self.0.try_lock() else { return Err(Full) };
        if q.len() == 32 {
            return Err(Full);
        }
        q.push_back(r);
        Ok(())
    }
    pub fn pop(&self) -> Option<CaptureRequest> {
        self.0.try_lock().ok()?.pop_front()
    }
    pub fn dispatch(&self, policy: &mut Policy) -> Option<Effects> {
        self.pop().map(|r| policy.event(PolicyEvent::Request(r)))
    }
}

/// Validation/compilation occurs on the worker before this bounded UI handoff.
pub struct ConfigurationQueue(Mutex<VecDeque<PreparedConfig>>);
impl Default for ConfigurationQueue {
    fn default() -> Self {
        Self::new()
    }
}
impl ConfigurationQueue {
    pub fn new() -> Self {
        Self(Mutex::new(VecDeque::with_capacity(32)))
    }
    pub fn try_push(&self, config: PreparedConfig) -> Result<(), Full> {
        let Ok(mut q) = self.0.try_lock() else { return Err(Full) };
        if q.len() == 32 {
            return Err(Full);
        }
        q.push_back(config);
        Ok(())
    }
    pub fn pop(&self) -> Option<PreparedConfig> {
        self.0.try_lock().ok()?.pop_front()
    }
}

use portable_atomic::AtomicU128;
use std::sync::atomic::{AtomicU64, Ordering};
/// Observations come from the actual state block, never from a request's claimed sequence.
/// Only the UI owner calls begin; other threads may publish observations without waiting.
pub struct StateSequence {
    latest: AtomicU64,
    applying: AtomicU128,
}
impl Default for StateSequence {
    fn default() -> Self {
        Self::new()
    }
}
impl StateSequence {
    pub const fn new() -> Self {
        Self { latest: AtomicU64::new(0), applying: AtomicU128::new(0) }
    }
    /// True means the caller must also revoke native publication tickets before returning.
    pub fn observe(&self, sequence: u64) -> bool {
        self.latest.fetch_max(sequence, Ordering::AcqRel) < sequence && !self.publication_allowed()
    }
    pub fn is_current(&self, sequence: Option<u64>) -> bool {
        sequence.is_none_or(|sequence| sequence == self.latest.load(Ordering::Acquire))
    }
    pub fn not_future(&self, sequence: Option<u64>) -> bool {
        sequence.is_none_or(|sequence| sequence <= self.latest.load(Ordering::Acquire))
    }
    pub fn publication_allowed(&self) -> bool {
        let applying = self.applying.load(Ordering::Acquire);
        applying == 0 || applying as u64 == self.latest.load(Ordering::Acquire)
    }
    pub fn begin(&self, sequence: Option<u64>) -> ApplyingSequence<'_> {
        let previous = self.applying.load(Ordering::Acquire);
        if previous == 0 {
            self.applying.store(sequence.map_or(0, |sequence| (1u128 << 64) | sequence as u128), Ordering::Release);
        }
        ApplyingSequence { sequence: self, previous, _owner: std::marker::PhantomData }
    }
}
#[must_use]
pub struct ApplyingSequence<'a> {
    sequence: &'a StateSequence,
    previous: u128,
    _owner: std::marker::PhantomData<*mut ()>,
}
impl Drop for ApplyingSequence<'_> {
    fn drop(&mut self) {
        self.sequence.applying.store(self.previous, Ordering::Release);
    }
}
