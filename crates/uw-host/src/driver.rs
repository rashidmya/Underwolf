//! The reconcile policy (design 2a "Components and flow"): the host keeps a desired state
//! (overlay path, `edit_mode`, `visible`) and, after every (re)connect, sends what makes the game
//! match it: `load_overlay` when `bootstrap_connected` says no overlay is loaded, and a full
//! `set_state` on every `overlay_connected`. A user's command updates the desired state as it is
//! sent, so a command that fails while the overlay is away still lands on the next connect.

use uw_pipe_proto::jsonl::{Command, Event, RoutingCommand, RoutingEvent};
use uw_pipe_proto::routing::{RoutingConfig, RoutingState};

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
    input_capable: bool,
    routing_desired: Option<RoutingConfig>,
    routing_state: Option<RoutingState>,
    routing_epoch: u64,
    routing_published_epoch: Option<u64>,
    routing_error: Option<&'static str>,
}

impl Driver {
    /// `ids` is shared with the host's user, so reconcile commands never reuse a user's id.
    pub fn new(desired: Desired, ids: RequestIds) -> Driver {
        Driver {
            desired,
            ids,
            input_capable: false,
            routing_desired: None,
            routing_state: None,
            routing_epoch: 0,
            routing_published_epoch: None,
            routing_error: None,
        }
    }

    pub fn take_routing_error(&mut self) -> Option<&'static str> {
        self.routing_error.take()
    }
    fn publish_routing(&mut self, state: RoutingState) -> Vec<Command> {
        if state.epoch < self.routing_epoch || state.epoch == self.routing_epoch && self.routing_state.is_none() {
            return vec![];
        }
        self.routing_epoch = state.epoch;
        self.routing_state = Some(state);
        self.desired.edit_mode = state.exclusive;
        if self.routing_published_epoch == Some(state.epoch) {
            return vec![];
        }
        let Some(mut config) = self.routing_desired.clone() else {
            return vec![];
        };
        let Some(revision) = config.revision.max(state.revision).checked_add(1) else {
            self.routing_error = Some("routing_revision_exhausted");
            return vec![];
        };
        let Some(request_id) = self.ids.try_next() else {
            self.routing_error = Some("routing_request_ids_exhausted");
            return vec![];
        };
        config.epoch = state.epoch;
        config.revision = revision;
        config.request_id = request_id;
        self.routing_desired = Some(config.clone());
        self.routing_published_epoch = Some(state.epoch);
        vec![Command::Routing(RoutingCommand::ConfigureInput { request_id, config })]
    }
    pub fn input_capable(&self) -> bool {
        self.input_capable
    }
    pub fn desired(&self) -> &Desired {
        &self.desired
    }

    /// A user's command is about to be sent: record what it asks for.
    pub fn note_command(&mut self, cmd: &Command) {
        match cmd {
            Command::Routing(command) => {
                if command.validate().is_err() {
                    self.routing_error = Some("routing_bad_message");
                    return;
                }
                if let RoutingCommand::ConfigureInput { config, .. } = command {
                    self.routing_desired = Some(config.clone());
                    self.routing_published_epoch =
                        self.routing_state.filter(|s| s.epoch == config.epoch).map(|s| s.epoch);
                }
            }
            Command::LoadOverlay { path, .. } => self.desired.overlay = Some(path.clone()),
            Command::SetState { edit_mode, visible, .. } => {
                if let Some(on) = edit_mode
                    && !self.input_capable
                {
                    self.desired.edit_mode = *on;
                }
                if let Some(on) = visible {
                    self.desired.visible = *on;
                }
            }
            Command::Ping { .. } | Command::InputAck { .. } => {}
        }
    }

    /// A helper event: the commands that bring the game to the desired state.
    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::Routing(RoutingEvent::RoutingReady { .. }) => {
                self.input_capable = true;
                self.desired.edit_mode = false;
                vec![]
            }
            Event::Routing(RoutingEvent::RoutingState { state }) => self.publish_routing(*state),
            Event::Routing(RoutingEvent::RoutingChallenge { challenge }) => {
                if let Some(state) = self.routing_state.filter(|state| state.epoch == challenge.epoch) {
                    self.publish_routing(state)
                } else {
                    vec![]
                }
            }
            Event::Error { request_id: Some(id), .. }
                if self.routing_desired.as_ref().is_some_and(|config| config.request_id == *id) =>
            {
                self.routing_published_epoch = None;
                vec![]
            }
            Event::Routing(RoutingEvent::RoutingConfigured { result })
                if result.code != uw_pipe_proto::routing::RoutingCode::Applied
                    && self.routing_desired.as_ref().is_some_and(|config| {
                        config.request_id == result.request_id
                            && config.epoch == result.epoch
                            && config.revision == result.revision
                    }) =>
            {
                self.routing_published_epoch = None;
                vec![]
            }
            Event::InputReady { .. } => {
                self.input_capable = true;
                self.desired.edit_mode = false;
                vec![]
            }
            Event::InputState { state } => {
                self.desired.edit_mode = state.active;
                vec![]
            }
            Event::OverlayDisconnected if self.input_capable => {
                self.desired.edit_mode = false;
                self.routing_state = None;
                self.routing_published_epoch = None;
                vec![]
            }
            Event::BootstrapConnected { overlay_loaded: false, .. } => match &self.desired.overlay {
                Some(path) => vec![Command::LoadOverlay { request_id: self.ids.next(), path: path.clone() }],
                None => vec![],
            },
            Event::OverlayConnected { .. } => vec![Command::SetState {
                input_epoch: None,
                request_id: self.ids.next(),
                edit_mode: (!self.input_capable).then_some(self.desired.edit_mode),
                visible: Some(self.desired.visible),
            }],
            _ => vec![],
        }
    }
}
