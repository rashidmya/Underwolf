//! The Linux-host <-> helper channel: one JSON object per line (design 2a "Linux ↔ helper").
//! Commands travel on the helper's stdin, events on its stdout (diagnostics on stderr). Every
//! command carries a `request_id`, and its one reply (`status`, `state_set`, `pong`, or an `error`)
//! echoes it. The game-data channel's events (design 2b: `data_connected`, `info`, `data_event`,
//! `data_lost`, `data_disconnected`) carry its `channel` and no `request_id`. `uw-host` adds its
//! own events (the last four `Event` variants) to the stream it hands its user. Serialized with
//! serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};
pub use crate::routing::json::{RoutingCommand, RoutingEvent};

use crate::data::SchemaCategory;
use crate::input::{CaptureCode, InputAck, InputBatch, InputChallenge, InputState};

/// Host -> helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    InputAck {
        request_id: u32,
        ack: InputAck,
    },
    /// Load (and start) the overlay at this Windows path. Reply: `status`.
    LoadOverlay {
        request_id: u32,
        path: String,
    },
    /// Set the given state fields. Reply: `state_set` with the block's `seq` (not bumped, and no
    /// bell, if nothing changed).
    SetState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_epoch: Option<u64>,
        request_id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit_mode: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible: Option<bool>,
    },
    /// Reply: `pong` from the overlay.
    Ping {
        request_id: u32,
    },
}

/// Helper -> host, and `uw-host`'s own events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    InputReady {
        version: u16,
    },
    InputState {
        state: InputState,
    },
    InputBatch {
        batch: InputBatch,
    },
    InputChallenge {
        challenge: InputChallenge,
    },
    CaptureResult {
        epoch: u64,
        state_seq: u64,
        capture_request_id: u32,
        code: CaptureCode,
    },
    InputAcked {
        request_id: u32,
    },
    /// The bootstrap connected and sent `hello`; `overlay_path` is the overlay it already runs.
    BootstrapConnected {
        overlay_loaded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overlay_path: Option<String>,
    },
    /// The bootstrap's reply to `load_overlay`: the lowercase code, a numeric `detail` (a Windows
    /// error or the start function's code, 0 if none) and a human message.
    Status {
        request_id: u32,
        code: String,
        detail: u32,
        message: String,
    },
    /// The reply to `set_state`.
    StateSet {
        request_id: u32,
        seq: u64,
    },
    /// The overlay's reply to `ping`: the last `seq` it applied.
    Pong {
        request_id: u32,
        seen_seq: u64,
    },
    /// The overlay connected to its pipe and sent `overlay_hello`.
    OverlayConnected {
        module_version: u32,
    },
    OverlayDisconnected,
    /// The overlay rang `bell-host`: the bits' names (unknown ones `bit<N>`), and `applied_seq`
    /// read right after.
    Bell {
        bits: Vec<String>,
        applied_seq: u64,
    },
    BootstrapDisconnected,
    /// The game process ended (the helper then exits 0).
    GameExited,
    /// Data channel `channel` was opened and checked (design 2b): its producer, the seq after
    /// which events will be delivered, and its schema. The snapshot's `info` lines follow.
    DataConnected {
        channel: u32,
        producer_id: u64,
        from_seq: u64,
        schema: Vec<SchemaCategory>,
    },
    /// One info key: the snapshot after `data_connected`, then each change. `value` null is a
    /// key set to null.
    Info {
        channel: u32,
        category: String,
        key: String,
        value: Option<String>,
        ts: u64,
    },
    /// One event from the ring, in seq order.
    DataEvent {
        channel: u32,
        seq: u64,
        name: String,
        data: String,
        ts: u64,
    },
    /// `events` were lost before the events that follow: overwritten before they were read, plus
    /// growth in the producer's dropped count.
    DataLost {
        channel: u32,
        events: u64,
    },
    /// The producer set `disconnect`.
    DataDisconnected {
        channel: u32,
    },
    /// A failure. `request_id` is set when it answers a command; `detail` carries a
    /// `GetLastError` where there is one (the channel id for `data_version_mismatch` /
    /// `data_bad_layout`).
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u32>,
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<u32>,
    },
    /// `uw-host`: it started the helper again (`attempt` counts restarts since the last reset).
    HelperRestarted {
        attempt: u32,
    },
    /// `uw-host`: a helper stdout line it could not parse.
    HelperProtocolError {
        line: String,
    },
    /// `uw-host`: `status ok` but no `overlay_connected` within 10 s.
    OverlayTimeout,
    /// `uw-host`: it stopped for good (no more restarts), and why.
    Stopped {
        reason: String,
    },
}

impl Event {
    /// An unsolicited error with no numeric detail.
    pub fn error(code: &str) -> Event {
        Event::Error { request_id: None, code: code.to_string(), detail: None }
    }

    /// An error that answers the command `request_id`.
    pub fn reply_error(request_id: u32, code: &str) -> Event {
        Event::Error { request_id: Some(request_id), code: code.to_string(), detail: None }
    }

    /// A data channel error (`data_version_mismatch`, `data_bad_layout`): `detail` is the
    /// channel id (design 2b).
    pub fn data_error(code: &str, channel: u32) -> Event {
        Event::Error { request_id: None, code: code.to_string(), detail: Some(channel) }
    }

    /// The `request_id` a reply answers (`status`, `state_set`, `pong`, or an `error` that
    /// answers a command); `None` for an unsolicited event.
    pub fn request_id(&self) -> Option<u32> {
        match self {
            Event::InputAcked { request_id }
            | Event::Status { request_id, .. }
            | Event::StateSet { request_id, .. }
            | Event::Pong { request_id, .. } => Some(*request_id),
            Event::Error { request_id, .. } => *request_id,
            _ => None,
        }
    }

    /// As `request_id`, to put another id on the reply (the `uw-inject` CLI maps its user's ids).
    pub fn request_id_mut(&mut self) -> Option<&mut u32> {
        match self {
            Event::InputAcked { request_id }
            | Event::Status { request_id, .. }
            | Event::StateSet { request_id, .. }
            | Event::Pong { request_id, .. } => Some(request_id),
            Event::Error { request_id, .. } => request_id.as_mut(),
            _ => None,
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Event serializes")
    }
    pub fn parse(line: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl Command {
    pub fn request_id(&self) -> u32 {
        match self {
            Command::InputAck { request_id, .. }
            | Command::LoadOverlay { request_id, .. }
            | Command::SetState { request_id, .. }
            | Command::Ping { request_id } => *request_id,
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Command serializes")
    }
    pub fn parse(line: &str) -> Result<Command, serde_json::Error> {
        serde_json::from_str(line.trim())
    }

    /// Parse a line of the `uw-inject` CLI's stdin, where `request_id` is optional (design:
    /// "request ids optional, assigned if absent"). The command always gets `id`, an id the host
    /// assigned, so a user's id can never collide with one of the host's own; the user's id, if the
    /// line had one, is returned so the reply can carry it back.
    pub fn parse_with_id(line: &str, id: u32) -> Result<(Command, Option<u32>), serde_json::Error> {
        let mut v: serde_json::Value = serde_json::from_str(line.trim())?;
        let mut user = None;
        if let Some(obj) = v.as_object_mut() {
            if let Some(given) = obj.get("request_id") {
                user = Some(serde_json::from_value::<u32>(given.clone())?);
            }
            obj.insert("request_id".into(), id.into());
        }
        Ok((serde_json::from_value(v)?, user))
    }
}
