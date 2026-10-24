//! Pure capture state machine. Its effects are bounded and allocate nothing on a hook path.
use crate::text::Utf16Text;
use uw_pipe_proto::input::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Inactive,
    Entering,
    Capturing,
    Releasing,
    Unavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Enter { epoch: u64, reason: InputReason },
    Release { epoch: u64, reason: InputReason },
}
#[derive(Debug, Clone, Copy)]
pub enum PolicyEvent {
    Connected,
    Disconnected,
    Foreground(bool),
    Visible(bool),
    Healthy(bool),
    Installed(bool),
    Destroyed,
    Input(InputRecord),
    Char(u16),
    Request(CaptureRequest),
    SetupFinished { epoch: u64, success: bool },
    Restored { epoch: u64 },
    Failure(InputReason),
}
pub struct Effects {
    pub pass: bool,
    pub releases: [Option<InputRecord>; 261],
    pub host: [Option<InputRecord>; 2],
    pub transition: Option<Transition>,
    pub result: Option<CaptureResult>,
}
impl Default for Effects {
    fn default() -> Self {
        Self { pass: false, releases: [None; 261], host: [None; 2], transition: None, result: None }
    }
}
pub struct Policy {
    mode: Mode,
    epoch: u64,
    connected: bool,
    foreground: bool,
    visible: bool,
    healthy: bool,
    installed: bool,
    keys: [bool; 256],
    game_keys: [Option<InputRecord>; 256],
    suppressed_keys: [bool; 256],
    buttons: [bool; 5],
    game_buttons: [Option<InputRecord>; 5],
    suppressed_buttons: [bool; 5],
    pending: Option<CaptureRequest>,
    text: Utf16Text,
}
impl Default for Policy {
    fn default() -> Self {
        Self::new()
    }
}
impl Policy {
    pub fn new() -> Self {
        Self {
            mode: Mode::Inactive,
            epoch: 1,
            connected: false,
            foreground: false,
            visible: false,
            healthy: false,
            installed: false,
            keys: [false; 256],
            game_keys: [None; 256],
            suppressed_keys: [false; 256],
            buttons: [false; 5],
            game_buttons: [None; 5],
            suppressed_buttons: [false; 5],
            pending: None,
            text: Utf16Text::default(),
        }
    }
    pub fn active(&self) -> bool {
        self.mode == Mode::Capturing
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn modifiers(&self) -> u32 {
        let pair = |generic, left, right| self.keys[generic] || self.keys[left] || self.keys[right];
        (pair(16, 160, 161) as u32)
            | ((pair(17, 162, 163) as u32) << 1)
            | ((pair(18, 164, 165) as u32) << 2)
            | (((self.keys[91] || self.keys[92]) as u32) << 3)
    }
    fn eligible(&self) -> bool {
        self.connected
            && self.foreground
            && self.visible
            && self.healthy
            && self.installed
            && self.mode != Mode::Unavailable
    }
    fn bump(&mut self) {
        if let Some(n) = self.epoch.checked_add(1) {
            self.epoch = n
        } else {
            self.mode = Mode::Unavailable;
            self.installed = false
        }
    }
    fn release(&mut self, reason: InputReason, e: &mut Effects) {
        self.text.reset();
        if matches!(self.mode, Mode::Entering | Mode::Capturing) {
            self.mode = Mode::Releasing;
            self.bump();
            self.suppressed_keys = self.keys;
            self.suppressed_buttons = self.buttons;
            e.transition = Some(Transition::Release { epoch: self.epoch, reason });
            if let Some(r) = self.pending.take() {
                e.result = Some(result(r, CaptureCode::Superseded))
            }
        }
    }
    fn enter(&mut self, reason: InputReason, e: &mut Effects) {
        self.mode = Mode::Entering;
        let mut n = 0;
        for r in self.game_keys.iter_mut().chain(self.game_buttons.iter_mut()) {
            if let Some(mut r) = r.take() {
                r.flags &= !5;
                e.releases[n] = Some(r);
                n += 1;
            }
        }
        self.suppressed_keys = self.keys;
        self.suppressed_buttons = self.buttons;
        e.transition = Some(Transition::Enter { epoch: self.epoch, reason });
    }
    pub fn event(&mut self, event: PolicyEvent) -> Effects {
        let mut e = Effects::default();
        match event {
            PolicyEvent::Connected => {
                if !self.connected {
                    self.connected = true;
                    self.healthy = false;
                    self.bump();
                }
            }
            PolicyEvent::Disconnected => {
                if self.connected {
                    self.connected = false;
                    self.healthy = false;
                    let old = self.epoch;
                    self.release(InputReason::Disconnected, &mut e);
                    if old == self.epoch {
                        self.bump();
                    }
                }
            }
            PolicyEvent::Foreground(on) => {
                let lost = self.foreground && !on;
                self.foreground = on;
                if lost && self.mode == Mode::Inactive {
                    self.bump();
                }
                if !on {
                    self.release(InputReason::FocusLost, &mut e)
                }
            }
            PolicyEvent::Visible(on) => {
                let lost = self.visible && !on;
                self.visible = on;
                if lost && self.mode == Mode::Inactive {
                    self.bump();
                }
                if !on {
                    self.release(InputReason::Hidden, &mut e)
                }
            }
            PolicyEvent::Healthy(on) => {
                let lost = self.healthy && !on;
                self.healthy = on;
                if lost && self.mode == Mode::Inactive {
                    self.bump();
                }
                if !on {
                    self.release(InputReason::LeaseExpired, &mut e)
                }
            }
            PolicyEvent::Installed(on) => {
                let lost = self.installed && !on;
                self.installed = on;
                if lost && self.mode == Mode::Inactive {
                    self.bump();
                }
                if !on {
                    self.release(InputReason::SetupFailed, &mut e)
                }
            }
            PolicyEvent::Destroyed => {
                self.foreground = false;
                self.installed = false;
                self.release(InputReason::WindowDestroyed, &mut e)
            }
            PolicyEvent::Failure(reason) => {
                self.healthy = false;
                self.release(reason, &mut e)
            }
            PolicyEvent::Request(r) => {
                if r.epoch != self.epoch {
                    e.result = Some(result(r, CaptureCode::StaleEpoch))
                } else if r.on {
                    if !self.eligible() || matches!(self.mode, Mode::Entering | Mode::Releasing) {
                        e.result = Some(result(r, CaptureCode::Unavailable))
                    } else if self.active() {
                        e.result = Some(result(r, CaptureCode::Applied))
                    } else {
                        self.pending = Some(r);
                        self.enter(InputReason::Requested, &mut e)
                    }
                } else if matches!(self.mode, Mode::Entering | Mode::Capturing) {
                    self.release(InputReason::Requested, &mut e);
                    self.pending = Some(r)
                } else if self.mode == Mode::Releasing {
                    e.result = Some(result(r, CaptureCode::Unavailable))
                } else {
                    e.result = Some(result(r, CaptureCode::Applied))
                }
            }
            PolicyEvent::SetupFinished { epoch, success } => {
                if epoch == self.epoch && self.mode == Mode::Entering {
                    if success && self.eligible() {
                        self.mode = Mode::Capturing;
                        if let Some(r) = self.pending.take() {
                            e.result = Some(result(r, CaptureCode::Applied));
                        }
                    } else {
                        let pending = self.pending.take();
                        self.release(InputReason::SetupFailed, &mut e);
                        if let Some(r) = pending {
                            e.result = Some(result(r, CaptureCode::SetupFailed))
                        }
                    }
                }
            }
            PolicyEvent::Restored { epoch } => {
                if epoch == self.epoch && self.mode == Mode::Releasing {
                    self.mode = Mode::Inactive;
                    if let Some(r) = self.pending.take() {
                        e.result = Some(result(r, CaptureCode::Applied))
                    }
                }
            }
            PolicyEvent::Char(unit) => {
                if self.active() {
                    e.host = self.text.push(unit).map(|c| c.map(|c| InputRecord::text(c, self.modifiers())))
                } else {
                    e.pass = self.mode == Mode::Inactive
                }
            }
            PolicyEvent::Input(r) => {
                if r.validate().is_err() {
                    self.release(InputReason::Unavailable, &mut e);
                    return e;
                }
                let down = r.flags & 1 != 0;
                let mut quarantined = false;
                if r.kind == 1 {
                    let k = r.a as usize;
                    let rising = down && !self.keys[k];
                    self.keys[k] = down;
                    quarantined = self.suppressed_keys[k];
                    if !down {
                        self.suppressed_keys[k] = false;
                    }
                    if k == 9 && rising && self.modifiers() & 2 != 0 {
                        self.suppressed_keys[k] = true;
                        if self.active() {
                            self.release(InputReason::Hotkey, &mut e)
                        } else if self.mode == Mode::Inactive && self.eligible() {
                            self.enter(InputReason::Hotkey, &mut e)
                        }
                        return e;
                    }
                } else if r.kind == 4 {
                    let b = r.a as usize;
                    self.buttons[b] = down;
                    quarantined = self.suppressed_buttons[b];
                    if !down {
                        self.suppressed_buttons[b] = false;
                    }
                }
                if self.active() {
                    if !quarantined {
                        e.host[0] = Some(r);
                    }
                } else if matches!(self.mode, Mode::Inactive | Mode::Unavailable) && !quarantined {
                    e.pass = true;
                    if r.kind == 1 {
                        self.game_keys[r.a as usize] = down.then_some(r)
                    }
                    if r.kind == 4 {
                        self.game_buttons[r.a as usize] = down.then_some(r)
                    }
                }
            }
        }
        e
    }
}
fn result(r: CaptureRequest, code: CaptureCode) -> CaptureResult {
    CaptureResult { epoch: r.epoch, state_seq: r.state_seq, request_id: r.request_id, code }
}
