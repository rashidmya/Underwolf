//! V2 counters use decimal strings, preserving every u64 bit across JavaScript.
use super::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
pub const MAX_JSON_BYTES: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingProgress {
    #[serde(with = "counter")]
    pub epoch: u64,
    #[serde(with = "counter")]
    pub nonce: u64,
    #[serde(with = "counter")]
    pub through_seq: u64,
}
impl From<InputChallenge> for RoutingProgress {
    fn from(c: InputChallenge) -> Self {
        Self { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq }
    }
}
impl From<InputAck> for RoutingProgress {
    fn from(c: InputAck) -> Self {
        Self { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq }
    }
}
impl From<RoutingProgress> for InputChallenge {
    fn from(c: RoutingProgress) -> Self {
        Self { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq }
    }
}
impl From<RoutingProgress> for InputAck {
    fn from(c: RoutingProgress) -> Self {
        Self { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutingCommand {
    ConfigureInput { request_id: u32, config: RoutingConfig },
    RouteRequest { request_id: u32, request: RoutingRequest },
    RoutingAck { request_id: u32, ack: RoutingProgress },
}
impl RoutingCommand {
    pub fn request_id(&self) -> u32 {
        match self {
            Self::ConfigureInput { request_id, .. }
            | Self::RouteRequest { request_id, .. }
            | Self::RoutingAck { request_id, .. } => *request_id,
        }
    }
    pub fn validate(&self) -> Result<(), ProtoError> {
        check(self.request_id() != 0)?;
        match self {
            Self::ConfigureInput { request_id, config } => {
                check(*request_id == config.request_id)?;
                config.validate()
            }
            Self::RouteRequest { request_id, request } => {
                check(*request_id == request.request_id)?;
                RoutingMessage::Request(*request).validate()
            }
            Self::RoutingAck { ack, .. } => RoutingMessage::Ack((*ack).into()).validate(),
        }
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        bounded(line)?;
        let result: Self = serde_json::from_str(line)?;
        result.validate().map_err(|_| serde_json::Error::custom("invalid routing command"))?;
        Ok(result)
    }
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        self.validate().map_err(|_| serde_json::Error::custom("invalid routing command"))?;
        let line = serde_json::to_string(self)?;
        bounded(&line)?;
        Ok(line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutingEvent {
    RoutingReady { version: u16 },
    RoutingState { state: RoutingState },
    RoutingConfigured { result: RoutingResult },
    RoutedBatch { batch: RoutedBatch },
    RoutingResult { result: RoutingResult },
    InputCoverage { coverage: NativeCoverage },
    RoutingChallenge { challenge: RoutingProgress },
    RoutingAcked { request_id: u32 },
}
impl RoutingEvent {
    pub fn validate(&self) -> Result<(), ProtoError> {
        match self {
            Self::RoutingReady { version } => check(*version == ROUTING_VERSION),
            Self::RoutingState { state } => RoutingMessage::State(*state).validate(),
            Self::RoutingConfigured { result } | Self::RoutingResult { result } => {
                RoutingMessage::Result(*result).validate()
            }
            Self::RoutedBatch { batch } => RoutingMessage::Events(batch.clone()).validate(),
            Self::InputCoverage { coverage } => RoutingMessage::Coverage(*coverage).validate(),
            Self::RoutingChallenge { challenge } => RoutingMessage::Challenge((*challenge).into()).validate(),
            Self::RoutingAcked { request_id } => check(*request_id != 0),
        }
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        bounded(line)?;
        let result: Self = serde_json::from_str(line)?;
        result.validate().map_err(|_| serde_json::Error::custom("invalid routing event"))?;
        Ok(result)
    }
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        self.validate().map_err(|_| serde_json::Error::custom("invalid routing event"))?;
        let line = serde_json::to_string(self)?;
        bounded(&line)?;
        Ok(line)
    }
}
fn bounded(line: &str) -> Result<(), serde_json::Error> {
    if line.len() > MAX_JSON_BYTES {
        Err(serde_json::Error::custom("routing JSON exceeds 32768 bytes"))
    } else {
        Ok(())
    }
}
pub fn parse_counter(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected canonical unsigned decimal counter");
    }
    value.parse().map_err(|_| "counter overflows u64")
}
pub mod counter {
    use super::*;
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        parse_counter(&text).map_err(D::Error::custom)
    }
}
pub mod optional_counter {
    use super::*;
    pub fn serialize<S: Serializer>(value: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        value.map(|v| v.to_string()).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        Option::<String>::deserialize(d)?.map(|s| parse_counter(&s).map_err(D::Error::custom)).transpose()
    }
}
