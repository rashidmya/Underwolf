//! The host's supervision rules (design 2a "Failures"), as a pure state machine on a millisecond
//! clock, so they are unit-tested with a scripted fake helper:
//! - every command to the helper is tracked by its request id until its reply; when the helper
//!   exits, or none is running, the command fails with `bootstrap_not_connected` (`load_overlay`)
//!   or `overlay_not_connected` (`set_state`, `ping`) — no queueing while disconnected;
//! - a helper that exits while the game runs is restarted after 1, 2, 4, 8 s, then every 10 s;
//!   the delay resets once a helper has been up 60 s (`helper_restarted {attempt}`);
//! - `game_exited`, `already_running`, `version_mismatch`, `hook_failed`, `bootstrap_timeout` and
//!   a failed start stop the host for good (`stopped {reason}`), with no restart;
//! - `status ok` without `overlay_connected` within 10 s is `overlay_timeout`;
//! - a stdout line that is not an event is `helper_protocol_error {line}`;
//! - after every (re)connect the `Driver` reconciles the desired state;
//! - per data channel it keeps a resume point (design 2b "uw-host"): the producer and the last seq
//!   delivered, from `data_connected` and each `data_event`. Every new helper gets
//!   `--data-resume <producer_id>:<seq>` for the channel it reads. Data events carry no request
//!   id, so they never touch pending commands, and they reach the user unchanged.

use std::collections::BTreeMap;

use uw_pipe_proto::data::{ResumePoint, DEFAULT_CHANNEL};
use uw_pipe_proto::jsonl::{Command, Event};

use crate::driver::{Desired, Driver};
use crate::RequestIds;

/// `status ok` -> `overlay_connected` deadline.
pub const OVERLAY_TIMEOUT_MS: u64 = 10_000;
/// Restart delays for the first four restarts; later ones wait `RESTART_DELAY_MAX_MS`.
pub const RESTART_DELAYS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];
pub const RESTART_DELAY_MAX_MS: u64 = 10_000;
/// A helper up this long resets the restart delay to the first one.
pub const UPTIME_RESET_MS: u64 = 60_000;
/// Helper errors that stop the host instead of a restart (design "Failures"): a restart would
/// only fail again, and for the hook errors would set a new hook on the game's UI thread.
pub const STOP_CODES: [&str; 4] = ["already_running", "version_mismatch", "hook_failed", "bootstrap_timeout"];

/// Something that happened to the host. Time passing is `Supervisor::tick`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A line the current helper wrote on stdout.
    HelperLine(String),
    /// The current helper exited (its stdout closed and it was reaped).
    HelperExited,
    /// A helper could not be started; the error code (`game_not_found`, `no_wine_session`,
    /// `helper_start_failed`).
    StartFailed(String),
    /// A command from the host's user, request id already assigned.
    User(Command),
    /// The host's user is going away: stop, without an event.
    Shutdown,
}

/// What the runtime must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start a helper with these extra arguments (`--data-resume` once a resume point is held).
    /// The runtime later reports `HelperExited`, or `StartFailed` at once.
    StartHelper { args: Vec<String> },
    /// Write this command to the current helper's stdin.
    Send(Command),
    /// Hand this event to the host's user.
    Emit(Event),
    /// Close the helper's stdin (it exits), and end once it has. Nothing restarts after this.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// A helper runs, started at this time.
    Running { since: u64 },
    /// No helper; the next one starts at this time.
    Waiting { restart_at: u64 },
    Stopped,
}

pub struct Supervisor {
    driver: Driver,
    phase: Phase,
    /// Restarts since the last reset.
    attempt: u32,
    /// Commands sent to the current helper and not answered yet.
    pending: Vec<Command>,
    overlay_up: bool,
    overlay_deadline: Option<u64>,
    /// Per data channel: the producer and the last seq delivered.
    resume: BTreeMap<u32, ResumePoint>,
}

/// The error a command gets when the side it needs is not there.
fn not_connected(cmd: &Command) -> Event {
    let code = match cmd {
        Command::LoadOverlay { .. } => "bootstrap_not_connected",
        Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
    };
    Event::reply_error(cmd.request_id(), code)
}

impl Supervisor {
    /// `ids` is the counter the host's user also takes ids from.
    pub fn new(desired: Desired, ids: RequestIds) -> Supervisor {
        Supervisor {
            driver: Driver::new(desired, ids),
            phase: Phase::Stopped,
            attempt: 0,
            pending: Vec::new(),
            overlay_up: false,
            overlay_deadline: None,
            resume: BTreeMap::new(),
        }
    }

    /// The first helper.
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        self.phase = Phase::Running { since: now };
        vec![Action::StartHelper { args: self.helper_args() }]
    }

    /// The resume point of data channel `channel`, once it delivered something.
    pub fn data_resume(&self, channel: u32) -> Option<ResumePoint> {
        self.resume.get(&channel).copied()
    }

    /// The arguments for a new helper: `--data-resume` for the channel it reads (the default
    /// one; `uw-host` passes no `--data-channel`), if a resume point is held.
    fn helper_args(&self) -> Vec<String> {
        match self.data_resume(DEFAULT_CHANNEL) {
            Some(r) => vec!["--data-resume".to_string(), r.to_string()],
            None => vec![],
        }
    }

    pub fn desired(&self) -> &Desired {
        self.driver.desired()
    }

    pub fn is_stopped(&self) -> bool {
        self.phase == Phase::Stopped
    }

    /// When `tick` next has something to do.
    pub fn next_deadline(&self) -> Option<u64> {
        let restart = match self.phase {
            Phase::Waiting { restart_at } => Some(restart_at),
            _ => None,
        };
        [restart, self.overlay_deadline].into_iter().flatten().min()
    }

    pub fn handle(&mut self, input: Input, now: u64) -> Vec<Action> {
        match input {
            Input::HelperLine(line) => match Event::parse(&line) {
                Ok(event) => self.on_event(event, now),
                Err(_) => vec![Action::Emit(Event::HelperProtocolError { line })],
            },
            Input::HelperExited => self.on_exit(now),
            Input::StartFailed(code) => {
                let mut out = vec![Action::Emit(Event::error(&code))];
                out.extend(self.stop(&code));
                out
            }
            Input::User(cmd) => {
                self.driver.note_command(&cmd);
                self.send(cmd)
            }
            Input::Shutdown => {
                if self.is_stopped() {
                    vec![]
                } else {
                    self.phase = Phase::Stopped;
                    self.overlay_deadline = None;
                    vec![Action::Stop]
                }
            }
        }
    }

    /// Restart a helper whose delay is over; report an overdue overlay.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if let Phase::Waiting { restart_at } = self.phase
            && now >= restart_at
        {
            self.phase = Phase::Running { since: now };
            out.push(Action::Emit(Event::HelperRestarted { attempt: self.attempt }));
            out.push(Action::StartHelper { args: self.helper_args() });
        }
        if self.overlay_deadline.is_some_and(|d| now >= d) {
            self.overlay_deadline = None;
            out.push(Action::Emit(Event::OverlayTimeout));
        }
        out
    }

    fn send(&mut self, cmd: Command) -> Vec<Action> {
        if matches!(self.phase, Phase::Running { .. }) {
            self.pending.push(cmd.clone());
            vec![Action::Send(cmd)]
        } else {
            vec![Action::Emit(not_connected(&cmd))]
        }
    }

    fn stop(&mut self, reason: &str) -> Vec<Action> {
        if self.is_stopped() {
            return vec![];
        }
        self.phase = Phase::Stopped;
        self.overlay_deadline = None;
        vec![Action::Emit(Event::Stopped { reason: reason.to_string() }), Action::Stop]
    }

    fn on_event(&mut self, event: Event, now: u64) -> Vec<Action> {
        if let Some(id) = event.request_id() {
            self.pending.retain(|c| c.request_id() != id);
        }
        match &event {
            Event::Status { code, .. } if code == "ok" && !self.overlay_up => {
                self.overlay_deadline = Some(now + OVERLAY_TIMEOUT_MS);
            }
            Event::OverlayConnected { .. } => {
                self.overlay_up = true;
                self.overlay_deadline = None;
            }
            Event::OverlayDisconnected => self.overlay_up = false,
            // The resume point (design 2b): `from_seq` on connect (a new producer replaces the
            // old one), then each event's seq.
            Event::DataConnected { channel, producer_id, from_seq, .. } => {
                self.resume.insert(*channel, ResumePoint { producer_id: *producer_id, seq: *from_seq });
            }
            Event::DataEvent { channel, seq, .. } => {
                if let Some(r) = self.resume.get_mut(channel) {
                    r.seq = *seq;
                }
            }
            _ => {}
        }
        let mut out = vec![Action::Emit(event.clone())];
        match &event {
            Event::GameExited => out.extend(self.stop("game_exited")),
            Event::Error { code, .. } if STOP_CODES.contains(&code.as_str()) => {
                out.extend(self.stop(code));
            }
            _ if !self.is_stopped() => {
                for cmd in self.driver.on_event(&event) {
                    out.extend(self.send(cmd));
                }
            }
            _ => {}
        }
        out
    }

    fn on_exit(&mut self, now: u64) -> Vec<Action> {
        let out: Vec<Action> = self.pending.drain(..).map(|c| Action::Emit(not_connected(&c))).collect();
        self.overlay_up = false;
        self.overlay_deadline = None;
        if let Phase::Running { since } = self.phase {
            if now.saturating_sub(since) >= UPTIME_RESET_MS {
                self.attempt = 0;
            }
            self.attempt += 1;
            let delay = RESTART_DELAYS_MS.get(self.attempt as usize - 1).copied().unwrap_or(RESTART_DELAY_MAX_MS);
            self.phase = Phase::Waiting { restart_at: now + delay };
        }
        out
    }
}
