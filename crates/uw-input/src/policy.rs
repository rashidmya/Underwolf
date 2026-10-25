//! Pure capture state machine. Its effects are bounded and allocate nothing on a hook path.
use crate::text::Utf16Text;
use uw_pipe_proto::input::*;
use uw_pipe_proto::routing::Domains;
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
    VisibleState { on: bool, state_seq: u64 },
    Healthy(bool),
    Installed(bool),
    Destroyed,
    Input(InputRecord),
    RoutedInput { record: InputRecord, pass: bool },
    Observed(InputRecord),
    Snapshot(InputRecord),
    Char(u16),
    Request(CaptureRequest),
    CaptureState { request: CaptureRequest, visible: bool, state_seq: u64 },
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
    pub applied: Option<u64>,
}
impl Default for Effects {
    fn default() -> Self {
        Self { pass: false, releases: [None; 261], host: [None; 2], transition: None, result: None, applied: None }
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
    pending_visibility: Option<u64>,
    text: Utf16Text,
    domains: Domains,
    legacy_hotkey: bool,
    notify_text: bool,
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
            pending_visibility: None,
            text: Utf16Text::default(),
            domains: Domains { keyboard: true, mouse: true },
            legacy_hotkey: true,
            notify_text: false,
        }
    }
    pub fn active(&self) -> bool {
        self.mode == Mode::Capturing
    }
    pub fn physical_keys(&self) -> [bool; 256] {
        self.keys
    }
    pub fn physical_buttons(&self) -> [bool; 5] {
        self.buttons
    }
    pub fn domains(&self) -> Domains {
        self.domains
    }
    pub fn use_configured_hotkeys(&mut self) {
        self.legacy_hotkey = false;
    }
    pub fn route_context(&self, healthy: bool, cursor_available: bool) -> crate::routing::RouteContext {
        crate::routing::RouteContext {
            epoch: self.epoch,
            foreground: self.foreground,
            visible: self.visible,
            healthy: healthy
                && self.connected
                && self.installed
                && !matches!(self.mode, Mode::Unavailable | Mode::Releasing),
            cursor_available,
            physical_keys: self.keys,
            physical_buttons: self.buttons,
        }
    }
    /// Apply desired routing domains; native setup still has to confirm entry.
    pub fn own_domains(&mut self, domains: Domains, healthy: bool) -> Effects {
        self.healthy = healthy;
        if !healthy && matches!(self.mode, Mode::Entering | Mode::Capturing) {
            let mut effects = Effects::default();
            self.release(InputReason::LeaseExpired, &mut effects);
            self.domains = domains;
            return effects;
        }
        let mut effects = self.set_domains(domains);
        if self.mode == Mode::Inactive && self.eligible() {
            self.enter(InputReason::Requested, &mut effects);
        }
        effects
    }
    pub fn reset_text(&mut self) {
        self.text.reset();
    }
    pub fn text_notifications(&mut self, enabled: bool) {
        if self.notify_text != enabled {
            self.text.reset();
        }
        self.notify_text = enabled;
    }
    /// The owner applies native effects before publishing the corresponding permission mask.
    pub fn set_domains(&mut self, domains: Domains) -> Effects {
        let old = self.domains;
        let mut effects = Effects::default();
        if matches!(self.mode, Mode::Entering | Mode::Capturing) {
            if domains == Domains::default() {
                self.release(InputReason::Requested, &mut effects);
            } else {
                self.quarantine(
                    Domains { keyboard: domains.keyboard && !old.keyboard, mouse: domains.mouse && !old.mouse },
                    &mut effects,
                );
                if old.keyboard && !domains.keyboard {
                    self.suppressed_keys = self.keys;
                }
                if old.mouse && !domains.mouse {
                    self.suppressed_buttons = self.buttons;
                }
            }
        }
        self.domains = domains;
        effects
    }
    pub fn key_suppressed(&self, vk: usize) -> bool {
        let held = |key: usize| self.suppressed_keys[key];
        match vk {
            16 => held(16) || held(160) || held(161),
            17 => held(17) || held(162) || held(163),
            18 => held(18) || held(164) || held(165),
            1 => self.suppressed_buttons[0],
            4 => self.suppressed_buttons[1],
            2 => self.suppressed_buttons[2],
            5 => self.suppressed_buttons[3],
            6 => self.suppressed_buttons[4],
            _ => self.suppressed_keys.get(vk).copied().unwrap_or(false),
        }
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
            && self.domains != Domains::default()
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
            if self.legacy_hotkey || reason != InputReason::Requested {
                self.bump();
            }
            if self.domains.keyboard {
                self.suppressed_keys = self.keys;
            }
            if self.domains.mouse {
                self.suppressed_buttons = self.buttons;
            }
            e.transition = Some(Transition::Release { epoch: self.epoch, reason });
            if let Some(r) = self.pending.take() {
                e.result = Some(result(r, CaptureCode::Superseded))
            }
        }
    }
    fn enter(&mut self, reason: InputReason, e: &mut Effects) {
        self.mode = Mode::Entering;
        self.quarantine(self.domains, e);
        e.transition = Some(Transition::Enter { epoch: self.epoch, reason });
    }
    fn quarantine(&mut self, domains: Domains, e: &mut Effects) {
        let mut n = 0;
        for (records, owned) in
            [(&mut self.game_keys[..], domains.keyboard), (&mut self.game_buttons[..], domains.mouse)]
        {
            if owned {
                for r in records {
                    if let Some(mut r) = r.take() {
                        r.flags &= !5;
                        e.releases[n] = Some(r);
                        n += 1;
                    }
                }
            }
        }
        if domains.keyboard {
            self.suppressed_keys = self.keys;
        }
        if domains.mouse {
            self.suppressed_buttons = self.buttons;
        }
    }
    /// The owner samples the current epoch's lease at execution, never at enqueue time.
    pub fn event_with_lease(&mut self, event: PolicyEvent, healthy: bool) -> Effects {
        if matches!(event, PolicyEvent::Healthy(true)) {
            return self.event(PolicyEvent::Healthy(healthy));
        }
        if matches!(
            event,
            PolicyEvent::Input(_)
                | PolicyEvent::RoutedInput { .. }
                | PolicyEvent::Observed(_)
                | PolicyEvent::Snapshot(_)
                | PolicyEvent::Request(_)
                | PolicyEvent::CaptureState { .. }
                | PolicyEvent::SetupFinished { .. }
        ) {
            self.healthy = healthy;
        }
        self.event(event)
    }
    pub fn event(&mut self, event: PolicyEvent) -> Effects {
        let mut e = Effects::default();
        match event {
            PolicyEvent::Connected => {
                if !self.connected {
                    self.connected = true;
                    self.healthy = false;
                    self.bump();
                    if self.mode == Mode::Releasing {
                        e.transition =
                            Some(Transition::Release { epoch: self.epoch, reason: InputReason::Disconnected });
                    }
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
                        if self.mode == Mode::Releasing {
                            e.transition =
                                Some(Transition::Release { epoch: self.epoch, reason: InputReason::Disconnected });
                        }
                    }
                    self.pending_visibility = None;
                    if let Some(request) = self.pending.take() {
                        e.result = Some(result(request, CaptureCode::Superseded));
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
            PolicyEvent::VisibleState { on, state_seq } => {
                e = self.event(PolicyEvent::Visible(on));
                if self.mode == Mode::Releasing {
                    self.pending_visibility = Some(state_seq);
                } else {
                    e.applied = Some(state_seq);
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
                if self.mode == Mode::Inactive {
                    self.bump();
                }
                self.healthy = false;
                self.foreground = false;
                self.installed = false;
                self.release(InputReason::WindowDestroyed, &mut e);
                self.keys = [false; 256];
                self.game_keys = [None; 256];
                self.suppressed_keys = [false; 256];
                self.buttons = [false; 5];
                self.game_buttons = [None; 5];
                self.suppressed_buttons = [false; 5];
                self.text.reset();
            }
            PolicyEvent::Failure(reason) => {
                self.healthy = false;
                self.release(reason, &mut e)
            }
            PolicyEvent::CaptureState { request, visible, state_seq } => {
                if request.epoch != self.epoch {
                    e.result = Some(result(request, CaptureCode::StaleEpoch));
                } else if request.state_seq != state_seq {
                    e.result = Some(result(request, CaptureCode::Superseded));
                } else {
                    // Validate the caller's epoch before visibility can invalidate it.
                    self.visible = visible;
                    e = self.event(PolicyEvent::Request(request));
                    if !visible {
                        self.release(InputReason::Hidden, &mut e);
                    }
                }
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
                    e.applied = self.pending_visibility.take();
                    if let Some(r) = self.pending.take() {
                        e.result = Some(result(r, CaptureCode::Applied))
                    }
                }
            }
            PolicyEvent::Char(unit) => {
                let owned = self.domains.keyboard && self.active();
                if owned || self.notify_text {
                    e.host = self.text.push(unit).map(|c| c.map(|c| InputRecord::text(c, self.modifiers())))
                }
                e.pass = !owned && (!self.domains.keyboard || matches!(self.mode, Mode::Inactive | Mode::Unavailable));
            }
            PolicyEvent::Snapshot(r) => {
                // Query APIs report generic modifier aliases as well as left/right
                // keys. Subsequent events use only left/right, so seed them once.
                if r.kind != 1 || !matches!(r.a, 16..=18) {
                    return self.event(PolicyEvent::Input(r));
                }
            }
            PolicyEvent::Observed(r) => {
                // Raw and legacy streams overlap. Raw reads track passthrough held
                // state/hotkeys, but never provide a second captured host event.
                let owned = if r.kind == 1 || r.kind == 2 { self.domains.keyboard } else { self.domains.mouse };
                if !self.active() || !owned {
                    return self.event(PolicyEvent::Input(r));
                }
            }
            PolicyEvent::RoutedInput { record, pass } => {
                let key = (record.kind == 1 && (1..=255).contains(&record.a)).then_some(record.a as usize);
                let button = (record.kind == 4 && (0..5).contains(&record.a)).then_some(record.a as usize);
                let old_key = key.and_then(|k| self.game_keys[k]);
                let old_button = button.and_then(|b| self.game_buttons[b]);
                e = self.event(PolicyEvent::Input(record));
                if !pass && e.pass {
                    if let Some(k) = key {
                        self.game_keys[k] = old_key;
                        if record.flags & 1 != 0 {
                            self.suppressed_keys[k] = true;
                        }
                    }
                    if let Some(b) = button {
                        self.game_buttons[b] = old_button;
                    }
                    e.pass = false;
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
                    if self.legacy_hotkey && k == 9 && rising && self.modifiers() & 2 != 0 {
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
                let owned = if r.kind == 1 || r.kind == 2 { self.domains.keyboard } else { self.domains.mouse };
                if self.active() && owned {
                    if !quarantined {
                        e.host[0] = Some(r);
                    }
                } else if (!owned || matches!(self.mode, Mode::Inactive | Mode::Unavailable)) && !quarantined {
                    e.pass = true;
                    if r.kind == 1 {
                        self.game_keys[r.a as usize] = down.then_some(r)
                    }
                    if r.kind == 4 {
                        self.game_buttons[r.a as usize] = down.then_some(r)
                    }
                }
                if r.kind == 2 && self.notify_text {
                    e.host[0] = Some(r);
                }
            }
        }
        e
    }
}
fn result(r: CaptureRequest, code: CaptureCode) -> CaptureResult {
    CaptureResult { epoch: r.epoch, state_seq: r.state_seq, request_id: r.request_id, code }
}
