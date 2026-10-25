//! Bounded, nonblocking hook output and receiver progress measured on the game's clock.
use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use uw_pipe_proto::input::*;
use uw_pipe_proto::routing::{EventKind, RoutedBatch, RoutedEvent, RoutingMessage, RoutingResult};
pub const QUEUE_RECORDS: usize = 1024;
pub const CONTROL_RECORDS: usize = 64;
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
        if let Ok(mut q) = self.queue.lock()
            && epoch > q.epoch
        {
            q.records.clear();
            q.epoch = epoch;
            q.next = 1;
            self.revoke.store(false, Ordering::Release)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedOutput {
    Events(RoutedBatch),
    Configured(RoutingResult),
}
#[derive(Clone, Copy)]
enum RoutedEntry {
    Event(RoutedEvent),
    Barrier(RoutingResult),
}
struct RoutedQueue {
    epoch: u64,
    next: u64,
    inputs: usize,
    controls: usize,
    entries: VecDeque<RoutedEntry>,
}
/// One ordered stream, with independently bounded space for control records.
pub struct RoutedOutbox {
    queue: Mutex<RoutedQueue>,
    revoke: AtomicBool,
}
impl Default for RoutedOutbox {
    fn default() -> Self {
        Self::new()
    }
}
impl RoutedOutbox {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(RoutedQueue {
                epoch: 0,
                next: 1,
                inputs: 0,
                controls: 0,
                entries: VecDeque::with_capacity(QUEUE_RECORDS + CONTROL_RECORDS),
            }),
            revoke: AtomicBool::new(false),
        }
    }
    pub fn revoked(&self) -> bool {
        self.revoke.load(Ordering::Acquire)
    }
    pub fn try_push(&self, epoch: u64, event: RoutedEvent) -> PushResult {
        self.try_group(epoch, &[event], None)
    }
    /// A router effect and its application barrier are accepted atomically. No allocation or wait.
    pub fn try_group(&self, epoch: u64, events: &[RoutedEvent], barrier: Option<RoutingResult>) -> PushResult {
        if events.len() > 35
            || events.iter().any(|e| e.validate().is_err())
            || barrier.is_some_and(|b| RoutingMessage::Configured(b).validate().is_err())
        {
            return self.fail();
        }
        let Ok(mut q) = self.queue.try_lock() else { return self.overflow(events, barrier) };
        if epoch == 0 || epoch != q.epoch {
            return PushResult::Stale;
        }
        let mut last = match q.entries.back() {
            Some(RoutedEntry::Event(e)) => Some(*e),
            _ => None,
        };
        let (mut inputs, mut controls) = (0, usize::from(barrier.is_some()));
        for event in events {
            if !last.is_some_and(|old| coalescible(old, *event)) {
                if event.kind == EventKind::Input {
                    inputs += 1;
                } else {
                    controls += 1;
                }
            }
            last = Some(*event);
        }
        if q.inputs + inputs > QUEUE_RECORDS || q.controls + controls > CONTROL_RECORDS {
            return self.overflow(events, barrier);
        }
        let mut coalesced = false;
        for event in events {
            if let Some(RoutedEntry::Event(old)) = q.entries.back_mut()
                && coalescible(*old, *event)
            {
                *old = *event;
                coalesced = true;
            } else {
                q.entries.push_back(RoutedEntry::Event(*event));
            }
        }
        if let Some(result) = barrier {
            q.entries.push_back(RoutedEntry::Barrier(result));
        }
        q.inputs += inputs;
        q.controls += controls;
        if coalesced && inputs == 0 && controls == 0 { PushResult::Coalesced } else { PushResult::Queued }
    }
    fn fail(&self) -> PushResult {
        self.revoke.store(true, Ordering::Release);
        PushResult::Revoke
    }
    fn overflow(&self, events: &[RoutedEvent], barrier: Option<RoutingResult>) -> PushResult {
        if barrier.is_none() && !events.is_empty() && events.iter().all(is_motion) {
            PushResult::DroppedMove
        } else {
            self.fail()
        }
    }
    /// Worker only; sequence numbers are assigned after coalescing, at emission.
    pub fn take(&self, epoch: u64) -> Option<RoutedOutput> {
        let mut q = self.queue.lock().ok()?;
        if epoch != q.epoch {
            return None;
        }
        match q.entries.front()? {
            RoutedEntry::Barrier(_) => {
                let RoutedEntry::Barrier(mut result) = q.entries.pop_front()? else { unreachable!() };
                q.controls -= 1;
                result.through_seq = q.next - 1;
                Some(RoutedOutput::Configured(result))
            }
            RoutedEntry::Event(_) => {
                let count = q.entries.iter().take(64).take_while(|e| matches!(e, RoutedEntry::Event(_))).count();
                let Some(next) = q.next.checked_add(count as u64) else {
                    self.fail();
                    return None;
                };
                let first_seq = q.next;
                let mut events = Vec::with_capacity(count);
                for _ in 0..count {
                    let RoutedEntry::Event(event) = q.entries.pop_front()? else { unreachable!() };
                    if event.kind == EventKind::Input {
                        q.inputs -= 1;
                    } else {
                        q.controls -= 1;
                    }
                    events.push(event);
                }
                q.next = next;
                Some(RoutedOutput::Events(RoutedBatch { epoch, first_seq, events }))
            }
        }
    }
    /// Worker only. New epochs discard old input and pending application results together.
    pub fn reset(&self, epoch: u64) {
        if let Ok(mut q) = self.queue.lock()
            && epoch > q.epoch
        {
            q.entries.clear();
            q.inputs = 0;
            q.controls = 0;
            q.epoch = epoch;
            q.next = 1;
            self.revoke.store(false, Ordering::Release);
        }
    }
}
fn is_motion(event: &RoutedEvent) -> bool {
    event.kind == EventKind::Input && event.input.is_some_and(|r| r.kind == 3)
}
fn coalescible(old: RoutedEvent, new: RoutedEvent) -> bool {
    is_motion(&old) && is_motion(&new) && old.target == new.target && old.revision == new.revision
}
/// Worker-owned progress keeps a sequence inseparable from its epoch.
#[derive(Default)]
pub struct SentProgress {
    epoch: u64,
    through: u64,
}
impl SentProgress {
    pub fn announce(&mut self, epoch: u64) {
        if epoch > self.epoch {
            self.epoch = epoch;
            self.through = 0;
        }
    }
    pub fn sent(&mut self, epoch: u64, through: u64) -> bool {
        if epoch != self.epoch || through < self.through {
            return false;
        }
        self.through = through;
        true
    }
    pub fn through(&self, epoch: u64) -> u64 {
        if epoch == self.epoch { self.through } else { 0 }
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
    pub fn healthy_in(&self, epoch: u64, now_ms: u64) -> bool {
        self.epoch == epoch && self.healthy(now_ms)
    }
    pub fn healthy(&self, now_ms: u64) -> bool {
        now_ms >= self.clock && self.accepted.is_some_and(|(_, at)| now_ms >= at && now_ms - at < LEASE_MS)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn routed(record: InputRecord) -> RoutedEvent {
        RoutedEvent {
            revision: 1,
            target: Some(uw_pipe_proto::routing::WidgetRef { id: 1, generation: 1 }),
            kind: EventKind::Input,
            argument: 0,
            input: Some(record),
        }
    }
    #[test]
    fn routed_contention_never_waits_or_loses_discrete_input_silently() {
        let q = RoutedOutbox::new();
        q.reset(1);
        let _held = q.queue.lock().unwrap();
        assert_eq!(q.try_push(1, routed(InputRecord::motion(1, 2, 0))), PushResult::DroppedMove);
        assert_eq!(q.try_push(1, routed(InputRecord::text('a', 0))), PushResult::Revoke);
        assert!(q.revoked());
    }
    #[test]
    fn routed_sequence_exhaustion_revokes_without_emitting_a_wrapped_batch() {
        let q = RoutedOutbox::new();
        q.reset(1);
        q.try_push(1, routed(InputRecord::text('a', 0)));
        q.queue.lock().unwrap().next = u64::MAX;
        assert!(q.take(1).is_none());
        assert!(q.revoked());
    }
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
