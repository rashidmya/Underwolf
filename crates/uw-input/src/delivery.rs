//! Bounded, nonblocking hook output and receiver progress measured on the game's clock.
use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use uw_pipe_proto::input::*;
pub const QUEUE_RECORDS: usize = 1024;
pub const HEARTBEAT_MS: u64 = 100;
pub const LEASE_MS: u64 = 500;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    Queued,
    Coalesced,
    DroppedMove,
    Revoke,
    Stale,
}
struct Queue {
    epoch: u64,
    next: u64,
    records: VecDeque<InputRecord>,
}
pub struct Outbox {
    queue: Mutex<Queue>,
    revoke: AtomicBool,
}
impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}
impl Outbox {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Queue { epoch: 0, next: 1, records: VecDeque::with_capacity(QUEUE_RECORDS) }),
            revoke: AtomicBool::new(false),
        }
    }
    pub fn revoked(&self) -> bool {
        self.revoke.load(Ordering::Acquire)
    }
    pub fn try_push(&self, epoch: u64, r: InputRecord) -> PushResult {
        if r.validate().is_err() {
            self.revoke.store(true, Ordering::Release);
            return PushResult::Revoke;
        }
        let Ok(mut q) = self.queue.try_lock() else { return self.overflow(r) };
        if epoch == 0 || epoch != q.epoch {
            return PushResult::Stale;
        }
        if r.kind == 3 && q.records.back().is_some_and(|last| last.kind == 3) {
            *q.records.back_mut().unwrap() = r;
            return PushResult::Coalesced;
        }
        if q.records.len() == QUEUE_RECORDS {
            return self.overflow(r);
        }
        q.records.push_back(r);
        PushResult::Queued
    }
    fn overflow(&self, r: InputRecord) -> PushResult {
        if r.kind == 3 {
            PushResult::DroppedMove
        } else {
            self.revoke.store(true, Ordering::Release);
            PushResult::Revoke
        }
    }
    /// Worker only; hooks never call this allocating/blocking consumer operation.
    pub fn take_batch(&self, epoch: u64) -> Option<InputBatch> {
        let mut q = self.queue.lock().ok()?;
        if q.epoch != epoch || q.records.is_empty() {
            return None;
        }
        let n = q.records.len().min(MAX_BATCH_RECORDS);
        let Some(next) = q.next.checked_add(n as u64) else {
            self.revoke.store(true, Ordering::Release);
            return None;
        };
        let first_seq = q.next;
        q.next = next;
        Some(InputBatch { epoch, first_seq, records: q.records.drain(..n).collect() })
    }
    /// Worker only. A stale reset must never erase a newer epoch's records.
    pub fn reset(&self, epoch: u64) {
        if let Ok(mut q) = self.queue.lock() {
            if epoch > q.epoch {
                q.records.clear();
                q.epoch = epoch;
                q.next = 1;
                self.revoke.store(false, Ordering::Release)
            }
        }
    }
}
#[derive(Clone, Copy)]
struct Issued {
    challenge: InputChallenge,
    at: u64,
}
pub struct Lease {
    epoch: u64,
    nonce: u64,
    issued: VecDeque<Issued>,
    last_sent: Option<u64>,
    accepted: Option<(u64, u64)>,
    last_through: u64,
    clock: u64,
}
impl Lease {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            nonce: 0,
            issued: VecDeque::with_capacity(8),
            last_sent: None,
            accepted: None,
            last_through: 0,
            clock: 0,
        }
    }
    pub fn reset(&mut self, epoch: u64) {
        if epoch > self.epoch {
            *self = Self::new(epoch)
        }
    }
    pub fn outstanding(&self) -> usize {
        self.issued.len()
    }
    pub fn challenge(&mut self, now_ms: u64, through_seq: u64) -> Option<InputChallenge> {
        if self.epoch == 0 || now_ms < self.clock || through_seq < self.last_through {
            return None;
        }
        self.clock = now_ms;
        if self.last_sent.is_some_and(|t| now_ms.saturating_sub(t) < HEARTBEAT_MS) {
            return None;
        }
        while self.issued.front().is_some_and(|c| now_ms.saturating_sub(c.at) >= LEASE_MS) {
            self.issued.pop_front();
        }
        if self.issued.len() == 8 {
            return None;
        }
        self.nonce = self.nonce.checked_add(1)?;
        let challenge = InputChallenge { epoch: self.epoch, nonce: self.nonce, through_seq };
        self.issued.push_back(Issued { challenge, at: now_ms });
        self.last_sent = Some(now_ms);
        self.last_through = through_seq;
        Some(challenge)
    }
    pub fn ack(&mut self, ack: InputAck, now_ms: u64) -> bool {
        if ack.epoch != self.epoch || now_ms < self.clock {
            return false;
        }
        self.clock = now_ms;
        let Some(index) = self
            .issued
            .iter()
            .position(|c| c.challenge.nonce == ack.nonce && c.challenge.through_seq == ack.through_seq)
        else {
            return false;
        };
        let issued = self.issued.remove(index).unwrap();
        if now_ms.saturating_sub(issued.at) >= LEASE_MS || self.accepted.is_some_and(|(nonce, _)| ack.nonce <= nonce) {
            return false;
        }
        self.accepted = Some((ack.nonce, issued.at));
        true
    }
    pub fn healthy(&self, now_ms: u64) -> bool {
        now_ms >= self.clock && self.accepted.is_some_and(|(_, at)| now_ms >= at && now_ms - at < LEASE_MS)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contended_hook_queue_never_waits() {
        let q = Outbox::new();
        q.reset(1);
        let _held = q.queue.lock().unwrap();
        assert_eq!(q.try_push(1, InputRecord::motion(1, 2, 0)), PushResult::DroppedMove);
        assert_eq!(q.try_push(1, InputRecord::key(65, 0, false, false, false, 0)), PushResult::Revoke);
        assert!(q.revoked());
    }
    #[test]
    fn sequence_and_nonce_exhaustion_never_wrap() {
        let q = Outbox::new();
        q.reset(1);
        q.try_push(1, InputRecord::text('a', 0));
        q.queue.lock().unwrap().next = u64::MAX;
        assert!(q.take_batch(1).is_none());
        assert!(q.revoked());
        let mut l = Lease::new(1);
        l.nonce = u64::MAX;
        assert!(l.challenge(0, 0).is_none());
        assert!(!l.healthy(0));
    }
}
