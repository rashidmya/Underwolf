//! The temporary Linux-host <-> helper channel: one JSON object per line. Commands travel on the
//! helper's stdin, events on its stdout (diagnostics on stderr). Plan 2's binary relay replaces
//! this. Serialized with serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};

/// Host -> helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Load (and start) the overlay at this Windows path.
    LoadOverlay { path: String },
}

/// Helper -> host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The bootstrap connected and sent `hello` (design step 7).
    BootstrapConnected { overlay_loaded: bool },
    /// A `status` reply from the bootstrap (design step 8): the lowercase code, a numeric `detail`
    /// (a Windows error or the start function's code, 0 if none) and a human message.
    Status { code: String, detail: u32, message: String },
    /// A helper- or host-side failure (e.g. `no_window`, `hook_failed`, `game_not_found`). `detail`
    /// carries a `GetLastError` where there is one (absent otherwise).
    Error {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<u32>,
    },
}

impl Event {
    /// An error event with no numeric detail.
    pub fn error(code: &str) -> Event {
        Event::Error { code: code.to_string(), detail: None }
    }
}

impl Command {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Command serializes")
    }
    pub fn parse(line: &str) -> Result<Command, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Event serializes")
    }
    pub fn parse(line: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}
