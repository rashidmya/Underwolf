//! The reconcile policy (design 2a "Components and flow"): the host keeps a desired state
//! (overlay path, `edit_mode`, `visible`) and, after every (re)connect, sends what makes the game
//! match it: `load_overlay` when `bootstrap_connected` says no overlay is loaded, and a full
//! `set_state` on every `overlay_connected`. A user's command updates the desired state as it is
//! sent, so a command that fails while the overlay is away still lands on the next connect.

use uw_pipe_proto::jsonl::{Command, Event};

use crate::RequestIds;

/// What the host wants the game to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    /// The overlay's Windows path; `None` to connect only.
    pub overlay: Option<String>,
    pub edit_mode: bool,
    pub visible: bool,
}

impl Desired {
    /// The overlay to load, visible and not in edit mode.
    pub fn new(overlay: Option<String>) -> Desired {
        Desired { overlay, edit_mode: false, visible: true }
    }
}

pub struct Driver {
    desired: Desired,
    ids: RequestIds,
}

impl Driver {
    /// `ids` is shared with the host's user, so reconcile commands never reuse a user's id.
    pub fn new(desired: Desired, ids: RequestIds) -> Driver {
        Driver { desired, ids }
    }

    pub fn desired(&self) -> &Desired {
        &self.desired
    }

    /// A user's command is about to be sent: record what it asks for.
    pub fn note_command(&mut self, cmd: &Command) {
        match cmd {
            Command::LoadOverlay { path, .. } => self.desired.overlay = Some(path.clone()),
            Command::SetState { edit_mode, visible, .. } => {
                if let Some(on) = edit_mode {
                    self.desired.edit_mode = *on;
                }
                if let Some(on) = visible {
                    self.desired.visible = *on;
                }
            }
            Command::Ping { .. } => {}
        }
    }

    /// A helper event: the commands that bring the game to the desired state.
    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded: false, .. } => match &self.desired.overlay {
                Some(path) => vec![Command::LoadOverlay { request_id: self.ids.next(), path: path.clone() }],
                None => vec![],
            },
            Event::OverlayConnected { .. } => vec![Command::SetState {
                request_id: self.ids.next(),
                edit_mode: Some(self.desired.edit_mode),
                visible: Some(self.desired.visible),
            }],
            _ => vec![],
        }
    }
}
