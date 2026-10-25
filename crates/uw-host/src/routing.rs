//! V2 progress advances only when the application confirms actual consumption.
use crate::input::InputError;
use std::collections::VecDeque;
use uw_pipe_proto::{
    input::{InputAck, InputChallenge},
    jsonl::{Event, RoutingEvent},
    routing::*,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingReceiverAction {
    Reset(RoutingState),
    State(RoutingState),
    Records(RoutedBatch),
    Configured(RoutingResult),
    Result(RoutingResult),
    Coverage(NativeCoverage),
    Challenge(InputChallenge),
    Deferred,
    Ignore,
}
#[derive(Default)]
pub struct RoutingReceiver {
    epoch: u64,
    connected: bool,
    received: u64,
    consumed: u64,
    barriers: VecDeque<(bool, RoutingResult)>,
}
impl RoutingReceiver {
    pub fn event(&mut self, event: &Event) -> Result<RoutingReceiverAction, InputError> {
        let routing = match event {
            Event::Routing(routing) => routing,
            Event::GameExited | Event::Stopped { .. } => {
                *self = Self::default();
                return Ok(RoutingReceiverAction::Ignore);
            }
            Event::OverlayDisconnected => {
                self.connected = false;
                self.received = 0;
                self.consumed = 0;
                self.barriers.clear();
                return Ok(RoutingReceiverAction::Ignore);
            }
            _ => return Ok(RoutingReceiverAction::Ignore),
        };
        routing.validate().map_err(|_| InputError)?;
        match routing {
            RoutingEvent::RoutingState { state } => {
                if state.epoch < self.epoch || !self.connected && state.epoch == self.epoch {
                    return Ok(RoutingReceiverAction::Ignore);
                }
                if state.epoch > self.epoch {
                    self.epoch = state.epoch;
                    self.connected = true;
                    self.received = 0;
                    self.consumed = 0;
                    self.barriers.clear();
                    Ok(RoutingReceiverAction::Reset(*state))
                } else {
                    Ok(RoutingReceiverAction::State(*state))
                }
            }
            RoutingEvent::RoutedBatch { batch } => {
                if !self.connected || batch.epoch < self.epoch {
                    return Ok(RoutingReceiverAction::Ignore);
                }
                if batch.epoch != self.epoch || self.received.checked_add(1) != Some(batch.first_seq) {
                    return Err(InputError);
                }
                self.received = batch.first_seq.checked_add(batch.events.len() as u64 - 1).ok_or(InputError)?;
                Ok(RoutingReceiverAction::Records(batch.clone()))
            }
            RoutingEvent::RoutingConfigured { result } | RoutingEvent::RoutingResult { result } => {
                if !self.connected || result.epoch < self.epoch {
                    return Ok(RoutingReceiverAction::Ignore);
                }
                if result.epoch != self.epoch || result.through_seq > self.received {
                    return Err(InputError);
                }
                let config = matches!(routing, RoutingEvent::RoutingConfigured { .. });
                if result.through_seq > self.consumed || !self.barriers.is_empty() {
                    if self.barriers.iter().filter(|(kind, _)| *kind == config).count() >= 32 {
                        return Err(InputError);
                    }
                    self.barriers.push_back((config, *result));
                    Ok(RoutingReceiverAction::Deferred)
                } else {
                    Ok(Self::barrier(config, *result))
                }
            }
            RoutingEvent::RoutingChallenge { challenge } => {
                if !self.connected || challenge.epoch < self.epoch {
                    return Ok(RoutingReceiverAction::Ignore);
                }
                if challenge.epoch != self.epoch || challenge.through_seq > self.received {
                    return Err(InputError);
                }
                Ok(RoutingReceiverAction::Challenge((*challenge).into()))
            }
            RoutingEvent::InputCoverage { coverage } => {
                if !self.connected || coverage.epoch < self.epoch {
                    return Ok(RoutingReceiverAction::Ignore);
                }
                if coverage.epoch != self.epoch {
                    return Err(InputError);
                }
                Ok(RoutingReceiverAction::Coverage(*coverage))
            }
            _ => Ok(RoutingReceiverAction::Ignore),
        }
    }
    fn barrier(config: bool, result: RoutingResult) -> RoutingReceiverAction {
        if config { RoutingReceiverAction::Configured(result) } else { RoutingReceiverAction::Result(result) }
    }
    pub fn consumed(&mut self, through_seq: u64) -> Result<Vec<RoutingReceiverAction>, InputError> {
        if !self.connected || through_seq < self.consumed || through_seq > self.received {
            return Err(InputError);
        }
        self.consumed = through_seq;
        let mut actions = Vec::new();
        while self.barriers.front().is_some_and(|(_, result)| result.through_seq <= through_seq) {
            let (config, result) = self.barriers.pop_front().ok_or(InputError)?;
            actions.push(Self::barrier(config, result));
        }
        Ok(actions)
    }
    pub fn acknowledge(&self, challenge: InputChallenge) -> Option<InputAck> {
        (self.connected
            && challenge.epoch != 0
            && challenge.epoch == self.epoch
            && challenge.nonce != 0
            && challenge.through_seq <= self.consumed)
            .then_some(InputAck { epoch: challenge.epoch, nonce: challenge.nonce, through_seq: challenge.through_seq })
    }
}
