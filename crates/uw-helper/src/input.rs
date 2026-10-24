//! Validation at the helper's input boundary. No input event enters the game-data ring.
use uw_pipe_proto::{input::*, jsonl::Event};
#[derive(Default)]
pub struct InputRelay {
    pub state: Option<InputState>,
    sequence: u64,
}
impl InputRelay {
    pub fn request_epoch(&self, epoch: Option<u64>) -> Result<u64, &'static str> {
        match (self.state, epoch) {
            (Some(s), Some(epoch)) if s.epoch == epoch => Ok(epoch),
            _ => Err("input_stale_epoch"),
        }
    }
    pub fn accept(&mut self, m: InputMessage) -> Result<Event, &'static str> {
        m.validate().map_err(|_| "input_bad_message")?;
        match m {
            InputMessage::State(state) => {
                if self.state.is_some_and(|s| state.epoch < s.epoch) {
                    return Err("input_stale_epoch");
                }
                if self.state.is_none_or(|s| s.epoch != state.epoch) {
                    self.sequence = 0
                }
                self.state = Some(state);
                Ok(Event::InputState { state })
            }
            InputMessage::Batch(batch) => {
                if self.request_epoch(Some(batch.epoch)).is_err()
                    || self.sequence.checked_add(1) != Some(batch.first_seq)
                {
                    return Err("input_bad_sequence");
                }
                self.sequence = batch.first_seq + batch.records.len() as u64 - 1;
                Ok(Event::InputBatch { batch })
            }
            InputMessage::Challenge(challenge) => {
                if self.request_epoch(Some(challenge.epoch)).is_err() || challenge.through_seq > self.sequence {
                    return Err("input_bad_sequence");
                }
                Ok(Event::InputChallenge { challenge })
            }
            InputMessage::Result(r) => Ok(Event::CaptureResult {
                epoch: r.epoch,
                state_seq: r.state_seq,
                capture_request_id: r.request_id,
                code: r.code,
            }),
            _ => Err("input_bad_message"),
        }
    }
}

/// Acceptance and application are separate; retain at most 32 in-flight applications.
#[derive(Default)]
pub struct CapturePending {
    entries: Vec<(CaptureRequest, u64)>,
}
impl CapturePending {
    pub fn available(&self, id: u32) -> bool {
        self.entries.len() < 32 && !self.entries.iter().any(|(r, _)| r.request_id == id)
    }
    pub fn add(&mut self, request: CaptureRequest, now: u64) -> Result<(), &'static str> {
        if !self.available(request.request_id) {
            return Err("input_requests_full");
        }
        self.entries.push((request, now.saturating_add(5000)));
        Ok(())
    }
    pub fn take(&mut self, result: CaptureResult) -> bool {
        let Some(i) = self.entries.iter().position(|(r, _)| {
            r.request_id == result.request_id && r.epoch == result.epoch && r.state_seq == result.state_seq
        }) else {
            return false;
        };
        self.entries.remove(i);
        true
    }
    pub fn expire(&mut self, now: u64) -> Vec<u32> {
        let mut expired = Vec::new();
        self.entries.retain(|(r, deadline)| {
            if *deadline > now {
                true
            } else {
                expired.push(r.request_id);
                false
            }
        });
        expired
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|(_, deadline)| *deadline).min()
    }
    pub fn drain(&mut self) -> Vec<u32> {
        self.entries.drain(..).map(|(r, _)| r.request_id).collect()
    }
}
