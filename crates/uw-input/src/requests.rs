//! Bounded cross-thread requests. Enqueue is not application; the UI dispatches later.
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
