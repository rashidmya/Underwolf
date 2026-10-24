//! Receiver progress belongs to the application consuming events, never to an intermediate relay.
use uw_pipe_proto::{input::*, jsonl::Event};
#[derive(Debug)]
pub struct InputError;
#[derive(Debug)]
pub enum ReceiverAction {
    Reset(InputState),
    State(InputState),
    Records(InputBatch),
    Challenge(InputChallenge),
    Ignore,
}
#[derive(Default)]
pub struct ReceiverState {
    epoch: u64,
    connected: bool,
    received: u64,
    consumed: u64,
}
impl ReceiverState {
    pub fn event(&mut self, event: &Event) -> Result<ReceiverAction, InputError> {
        match event {
            Event::InputState { state: s } => {
                if s.epoch == 0 {
                    return Err(InputError);
                }
                if s.epoch < self.epoch || (!self.connected && s.epoch == self.epoch) {
                    return Ok(ReceiverAction::Ignore);
                }
                if s.epoch > self.epoch {
                    self.connected = true;
                    self.epoch = s.epoch;
                    self.received = 0;
                    self.consumed = 0;
                    Ok(ReceiverAction::Reset(*s))
                } else {
                    Ok(ReceiverAction::State(*s))
                }
            }
            Event::InputBatch { batch: b } => {
                if !self.connected || b.epoch < self.epoch {
                    return Ok(ReceiverAction::Ignore);
                }
                if b.epoch != self.epoch
                    || self.received.checked_add(1) != Some(b.first_seq)
                    || InputMessage::Batch(b.clone()).validate().is_err()
                {
                    return Err(InputError);
                }
                self.received = b.first_seq + b.records.len() as u64 - 1;
                Ok(ReceiverAction::Records(b.clone()))
            }
            Event::InputChallenge { challenge: c } => {
                if !self.connected || c.epoch < self.epoch {
                    return Ok(ReceiverAction::Ignore);
                }
                if c.epoch != self.epoch || c.nonce == 0 || c.through_seq > self.received {
                    return Err(InputError);
                }
                Ok(ReceiverAction::Challenge(*c))
            }
            Event::GameExited | Event::Stopped { .. } => {
                *self = Self::default();
                Ok(ReceiverAction::Ignore)
            }
            Event::OverlayDisconnected => {
                self.connected = false;
                self.received = 0;
                self.consumed = 0;
                Ok(ReceiverAction::Ignore)
            }
            _ => Ok(ReceiverAction::Ignore),
        }
    }
    pub fn consumed(&mut self, through_seq: u64) -> Result<(), InputError> {
        if through_seq < self.consumed || through_seq > self.received {
            return Err(InputError);
        }
        self.consumed = through_seq;
        Ok(())
    }
    pub fn acknowledge(&self, c: InputChallenge) -> Option<InputAck> {
        (self.connected && c.epoch != 0 && c.epoch == self.epoch && c.through_seq <= self.consumed)
            .then_some(InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq })
    }
}
