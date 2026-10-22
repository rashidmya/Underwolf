//! Turns the helper's JSON-lines events into the commands the host sends back. The only policy in
//! this plan: when the bootstrap connects fresh (no overlay yet) and an overlay path is
//! configured, send `load_overlay` once; after a client restart the bootstrap reports
//! `overlay_loaded: true`, so nothing is re-sent (design "Reconnect after a client restart").

use uw_pipe_proto::jsonl::{Command, Event};

pub struct Driver {
    overlay: Option<String>,
    sent_load: bool,
    connected: bool,
}

impl Driver {
    /// `overlay` is the Windows path to load, or `None` to connect only.
    pub fn new(overlay: Option<String>) -> Driver {
        Driver { overlay, sent_load: false, connected: false }
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded } => {
                self.connected = true;
                if *overlay_loaded {
                    // The pinned bootstrap already has the overlay (client restart): nothing to do.
                    self.sent_load = true;
                }
                match &self.overlay {
                    Some(path) if !overlay_loaded && !self.sent_load => {
                        self.sent_load = true;
                        vec![Command::LoadOverlay { path: path.clone() }]
                    }
                    _ => vec![],
                }
            }
            Event::Status { .. } | Event::Error { .. } => vec![],
        }
    }

    /// True once the bootstrap has connected and nothing more needs sending.
    pub fn is_done(&self) -> bool {
        self.connected && (self.overlay.is_none() || self.sent_load)
    }
}
