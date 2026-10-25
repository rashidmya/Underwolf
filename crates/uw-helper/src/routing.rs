//! Ordered v2 relay and exact application correlation, independent of game-data transport.
use uw_pipe_proto::{
    jsonl::{Event, RoutingEvent},
    routing::*,
};
#[derive(Default)]
pub struct RoutingRelay {
    pub state: Option<RoutingState>,
    sequence: u64,
}
impl RoutingRelay {
    pub fn request_epoch(&self, epoch: u64) -> Result<(), &'static str> {
        if self.state.is_some_and(|s| s.epoch == epoch) { Ok(()) } else { Err("routing_stale_epoch") }
    }
    pub fn accept(&mut self, message: RoutingMessage) -> Result<Event, &'static str> {
        message.validate().map_err(|_| "routing_bad_message")?;
        let event = match message {
            RoutingMessage::State(state) => {
                if self.state.is_some_and(|old| state.epoch < old.epoch) {
                    return Err("routing_stale_epoch");
                }
                if self.state.is_none_or(|old| old.epoch != state.epoch) {
                    self.sequence = 0;
                }
                self.state = Some(state);
                RoutingEvent::RoutingState { state }
            }
            RoutingMessage::Events(batch) => {
                self.request_epoch(batch.epoch)?;
                if self.sequence.checked_add(1) != Some(batch.first_seq) {
                    return Err("routing_bad_sequence");
                }
                self.sequence =
                    batch.first_seq.checked_add(batch.events.len() as u64 - 1).ok_or("routing_bad_sequence")?;
                RoutingEvent::RoutedBatch { batch }
            }
            RoutingMessage::Configured(result) | RoutingMessage::Result(result) => {
                if result.code == RoutingCode::Applied || result.through_seq != 0 {
                    self.request_epoch(result.epoch)?;
                    if result.through_seq > self.sequence {
                        return Err("routing_bad_sequence");
                    }
                }
                if matches!(message, RoutingMessage::Configured(_)) {
                    RoutingEvent::RoutingConfigured { result }
                } else {
                    RoutingEvent::RoutingResult { result }
                }
            }
            RoutingMessage::Challenge(challenge) => {
                self.request_epoch(challenge.epoch)?;
                if challenge.through_seq > self.sequence {
                    return Err("routing_bad_sequence");
                }
                RoutingEvent::RoutingChallenge { challenge: challenge.into() }
            }
            RoutingMessage::Coverage(coverage) => {
                self.request_epoch(coverage.epoch)?;
                RoutingEvent::InputCoverage { coverage }
            }
            _ => return Err("routing_bad_message"),
        };
        Ok(Event::Routing(event))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Configure,
    Request,
}
struct Pending {
    purpose: Purpose,
    epoch: u64,
    revision: u64,
    id: u32,
    deadline: u64,
}
#[derive(Default)]
pub struct RoutingPending {
    entries: Vec<Pending>,
}
impl RoutingPending {
    pub fn available(&self, purpose: Purpose, id: u32) -> bool {
        id != 0
            && self.entries.iter().filter(|p| p.purpose == purpose).count() < 32
            && !self.entries.iter().any(|p| p.purpose == purpose && p.id == id)
    }
    pub fn add(&mut self, purpose: Purpose, epoch: u64, revision: u64, id: u32, now: u64) -> Result<(), &'static str> {
        if epoch == 0 || revision == 0 {
            return Err("routing_bad_message");
        }
        if !self.available(purpose, id) {
            return Err("routing_requests_full");
        }
        let deadline = now.checked_add(5000).ok_or("routing_counter_exhausted")?;
        self.entries.push(Pending { purpose, epoch, revision, id, deadline });
        Ok(())
    }
    pub fn take(&mut self, purpose: Purpose, result: &RoutingResult) -> bool {
        let Some(index) = self.entries.iter().position(|p| {
            p.purpose == purpose
                && p.epoch == result.epoch
                && p.revision == result.revision
                && p.id == result.request_id
        }) else {
            return false;
        };
        self.entries.remove(index);
        true
    }
    pub fn retire_before(&mut self, epoch: u64) -> Vec<(Purpose, u32)> {
        let mut lost = Vec::new();
        self.entries.retain(|pending| {
            if pending.epoch < epoch {
                lost.push((pending.purpose, pending.id));
                false
            } else {
                true
            }
        });
        lost
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|p| p.deadline).min()
    }
    pub fn expire(&mut self, now: u64) -> Vec<(Purpose, u32)> {
        let mut expired = Vec::new();
        self.entries.retain(|p| {
            if p.deadline > now {
                true
            } else {
                expired.push((p.purpose, p.id));
                false
            }
        });
        expired
    }
    pub fn drain(&mut self) -> Vec<(Purpose, u32)> {
        self.entries.drain(..).map(|p| (p.purpose, p.id)).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum First {
    State(u32),
    Routing(u32),
    VersionMismatch,
    Bad,
}
pub fn classify_first(bytes: &[u8]) -> First {
    use uw_pipe_proto::pipe::{OverlayMessage, ProtoError, decode_overlay};
    match decode_overlay(bytes) {
        Ok(OverlayMessage::Hello { module_version, state_layout: 1 }) => First::State(module_version),
        Ok(OverlayMessage::Hello { .. }) | Err(ProtoError::BadVersion(_)) => First::VersionMismatch,
        Ok(message) => match classify_hello(&message) {
            RoutingFirst::Hello { module_version } => First::Routing(module_version),
            RoutingFirst::VersionMismatch => First::VersionMismatch,
            RoutingFirst::Bad => First::Bad,
        },
        Err(_) => First::Bad,
    }
}
