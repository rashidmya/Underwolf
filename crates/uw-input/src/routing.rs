//! Bounded widget routing. Effects describe desired ownership; native setup commits actual permission.
use crate::{configuration::PreparedConfig, policy::Transition};
use uw_pipe_proto::{
    input::{InputReason, InputRecord},
    routing::*,
};
#[derive(Clone, Copy)]
pub struct RouteContext {
    pub epoch: u64,
    pub foreground: bool,
    pub visible: bool,
    pub healthy: bool,
    pub cursor_available: bool,
    pub physical_keys: [bool; 256],
    pub physical_buttons: [bool; 5],
}
pub struct RouteEffects {
    pub pass: bool,
    pub domains: Domains,
    pub exclusive: bool,
    pub events: [Option<RoutedEvent>; 35],
    pub transition: Option<Transition>,
    pub code: Option<RoutingCode>,
    overflow: bool,
}
impl Default for RouteEffects {
    fn default() -> Self {
        Self {
            pass: true,
            domains: Domains::default(),
            exclusive: false,
            events: [None; 35],
            transition: None,
            code: None,
            overflow: false,
        }
    }
}
impl RouteEffects {
    fn push(&mut self, event: RoutedEvent) {
        if event.kind == EventKind::Cancel
            && self.events.iter().flatten().any(|old| old.kind == EventKind::Cancel && old.target == event.target)
        {
            return;
        }
        if let Some(slot) = self.events.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(event);
        } else {
            self.overflow = true;
        }
    }
}
pub struct Router {
    widgets: [Option<WidgetSpec>; MAX_WIDGETS],
    width: u32,
    height: u32,
    epoch: u64,
    revision: u64,
    configured: bool,
    armed: bool,
    exclusive: bool,
    focus: Option<WidgetRef>,
    takeover: Option<WidgetRef>,
    pointer: Option<WidgetRef>,
    capture: Option<WidgetRef>,
    buttons: [bool; 5],
    button_blocked: [bool; 5],
    qbuttons: [bool; 5],
    keys: [Option<WidgetRef>; 256],
    key_blocked: [bool; 256],
    qkeys: [bool; 256],
    point: Option<(i32, i32)>,
}
impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}
impl Router {
    pub fn new() -> Self {
        Self {
            widgets: [None; MAX_WIDGETS],
            width: 0,
            height: 0,
            epoch: 0,
            revision: 0,
            configured: false,
            armed: false,
            exclusive: false,
            focus: None,
            takeover: None,
            pointer: None,
            capture: None,
            buttons: [false; 5],
            button_blocked: [false; 5],
            qbuttons: [false; 5],
            keys: [None; 256],
            key_blocked: [false; 256],
            qkeys: [false; 256],
            point: None,
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Owner calls after safety revocation, before accepting fresh input in the new epoch.
    pub fn advance_epoch(&mut self, epoch: u64) {
        if epoch > self.epoch {
            self.epoch = epoch;
        }
    }
    pub fn physical_release(&mut self, record: InputRecord) {
        if record.validate().is_err() || record.flags & 1 != 0 {
            return;
        }
        match record.kind {
            1 => self.qkeys[record.a as usize] = false,
            4 => self.qbuttons[record.a as usize] = false,
            _ => {}
        }
    }
    pub fn focus(&self) -> Option<WidgetRef> {
        self.focus
    }
    pub fn takeover(&self) -> Option<WidgetRef> {
        self.takeover
    }
    pub fn pointer(&self) -> Option<WidgetRef> {
        self.pointer
    }
    pub fn pointer_captured(&self) -> bool {
        self.capture.is_some()
    }
    pub fn exclusive(&self) -> bool {
        self.exclusive
    }
    pub fn domains(&self) -> Domains {
        if !self.configured {
            return Domains::default();
        }
        if self.exclusive {
            return Domains { keyboard: true, mouse: true };
        }
        let blocking = |target| self.widget(target).is_some_and(|w| w.mode == InputMode::Block);
        Domains {
            keyboard: blocking(self.focus),
            mouse: (self.armed || self.capture.is_some()) && blocking(self.capture.or(self.pointer)),
        }
    }
    fn widget(&self, target: Option<WidgetRef>) -> Option<WidgetSpec> {
        let target = target?;
        self.widgets.iter().flatten().find(|w| w.instance == target).copied()
    }
    fn eligible(w: WidgetSpec, width: u32, height: u32) -> bool {
        w.visible
            && w.width > 0
            && w.height > 0
            && w.x < i64::from(width).min(i32::MAX as i64) as i32
            && w.y < i64::from(height).min(i32::MAX as i64) as i32
            && i64::from(w.x) + i64::from(w.width) > 0
            && i64::from(w.y) + i64::from(w.height) > 0
    }
    fn hit(&self, x: i32, y: i32) -> Option<WidgetRef> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        self.widgets
            .iter()
            .flatten()
            .filter(|w| {
                Self::eligible(**w, self.width, self.height)
                    && (self.exclusive || w.mode != InputMode::Pass)
                    && x >= w.x
                    && y >= w.y
                    && i64::from(x) < i64::from(w.x) + i64::from(w.width)
                    && i64::from(y) < i64::from(w.y) + i64::from(w.height)
            })
            .max_by_key(|w| w.z)
            .map(|w| w.instance)
    }
    fn emit(&self, e: &mut RouteEffects, target: WidgetRef, kind: EventKind, argument: u32) {
        e.push(RoutedEvent { revision: self.revision, target: Some(target), kind, argument, input: None });
    }
    fn cancel(&mut self, target: WidgetRef, reason: InputReason, e: &mut RouteEffects) {
        self.emit(e, target, EventKind::Cancel, reason as u32);
        for (i, key) in self.keys.iter_mut().enumerate() {
            if *key == Some(target) {
                *key = None;
                self.qkeys[i] = true;
            }
        }
        if self.focus == Some(target) {
            self.emit(e, target, EventKind::Focus, 0);
            self.focus = None;
        }
        if self.capture == Some(target) {
            for i in 0..5 {
                self.qbuttons[i] |= self.buttons[i];
                self.buttons[i] = false;
            }
            self.capture = None;
        }
        if self.pointer == Some(target) {
            self.emit(e, target, EventKind::Leave, 0);
            self.pointer = None;
        }
    }
    fn clear(&mut self, reason: InputReason, e: &mut RouteEffects) {
        for target in [self.focus, self.capture, self.pointer, self.takeover].into_iter().flatten() {
            self.cancel(target, reason, e);
        }
        self.focus = None;
        self.capture = None;
        self.pointer = None;
        self.takeover = None;
        self.armed = false;
        self.exclusive = false;
    }
    fn finish(&mut self, old: Domains, epoch: u64, reason: InputReason, mut e: RouteEffects) -> RouteEffects {
        if e.overflow {
            self.armed = false;
            self.exclusive = false;
            self.focus = None;
            self.capture = None;
            self.pointer = None;
            self.takeover = None;
            e.events = [None; 35];
            e.code = Some(RoutingCode::Unavailable);
            e.transition = Some(Transition::Release { epoch, reason: InputReason::Overflow });
        }
        e.domains = self.domains();
        e.exclusive = self.exclusive;
        if e.transition.is_none() {
            if old == Domains::default() && e.domains != Domains::default() {
                e.transition = Some(Transition::Enter { epoch, reason });
            }
            if old != Domains::default() && e.domains == Domains::default() {
                e.transition = Some(Transition::Release { epoch, reason });
            }
        }
        e
    }
    pub fn configure(&mut self, config: &PreparedConfig) -> RouteEffects {
        let c = config.config();
        let old = self.domains();
        let mut e = RouteEffects::default();
        if c.epoch < self.epoch {
            e.code = Some(RoutingCode::StaleEpoch);
            return self.finish(old, self.epoch, InputReason::Requested, e);
        }
        if c.revision <= self.revision {
            e.code = Some(RoutingCode::StaleRevision);
            return self.finish(old, self.epoch, InputReason::Requested, e);
        }
        let changed_geometry = self.configured && (c.width != self.width || c.height != self.height);
        if changed_geometry || (self.epoch != 0 && c.epoch != self.epoch) {
            self.clear(InputReason::Requested, &mut e);
        }
        let mut owner_affected = false;
        for old_widget in self.widgets.into_iter().flatten() {
            let next = c.widgets.iter().find(|next| next.instance == old_widget.instance);
            let changed = next.is_none_or(|next| {
                !Self::eligible(*next, c.width, c.height)
                    || next.mode != old_widget.mode
                    || next.keyboard != old_widget.keyboard
            });
            if changed {
                owner_affected |= self.takeover == Some(old_widget.instance);
                if self.focus == Some(old_widget.instance)
                    || self.capture == Some(old_widget.instance)
                    || self.pointer == Some(old_widget.instance)
                {
                    self.armed = false;
                }
                self.cancel(old_widget.instance, InputReason::Requested, &mut e);
            }
        }
        if owner_affected {
            self.clear(InputReason::Requested, &mut e);
        }
        self.widgets = [None; MAX_WIDGETS];
        for (slot, w) in self.widgets.iter_mut().zip(&c.widgets) {
            *slot = Some(*w);
        }
        self.width = c.width;
        self.height = c.height;
        self.epoch = c.epoch;
        self.revision = c.revision;
        self.configured = true;
        if self.capture.is_none()
            && self.pointer.is_some()
            && self.point.is_some_and(|(x, y)| self.hit(x, y) != self.pointer)
        {
            if let Some(pointer) = self.pointer {
                self.emit(&mut e, pointer, EventKind::Leave, 0);
            }
            self.pointer = None;
        }
        e.code = Some(RoutingCode::Applied);
        self.finish(old, self.epoch, InputReason::Requested, e)
    }
    pub fn revoke(&mut self, reason: InputReason) -> RouteEffects {
        let old = self.domains();
        let mut e = RouteEffects::default();
        self.clear(reason, &mut e);
        if matches!(reason, InputReason::Disconnected | InputReason::WindowDestroyed) {
            self.configured = false;
        }
        self.finish(old, self.epoch, reason, e)
    }
    fn focus_to(&mut self, target: Option<WidgetRef>, context: RouteContext, e: &mut RouteEffects) {
        if self.focus == target {
            return;
        }
        if let Some(old) = self.focus {
            self.cancel(old, InputReason::Requested, e);
        }
        self.focus = target;
        if let Some(target) = target {
            for (q, held) in self.qkeys.iter_mut().zip(context.physical_keys) {
                *q = held;
            }
            self.emit(e, target, EventKind::Focus, 1);
        }
    }
    pub fn request(&mut self, request: RoutingRequest, context: RouteContext) -> RouteEffects {
        let old = self.domains();
        let mut e = RouteEffects::default();
        if !context.foreground {
            e = self.revoke(InputReason::FocusLost);
        } else if !context.visible {
            e = self.revoke(InputReason::Hidden);
        } else if !context.healthy {
            e = self.revoke(InputReason::LeaseExpired);
        }
        let code = if RoutingMessage::Request(request).validate().is_err() {
            RoutingCode::Invalid
        } else if request.epoch != context.epoch {
            RoutingCode::StaleEpoch
        } else if !self.configured {
            RoutingCode::Unavailable
        } else if request.revision != self.revision {
            RoutingCode::StaleRevision
        } else {
            RoutingCode::Applied
        };
        if code != RoutingCode::Applied {
            e.code = Some(code);
            return self.finish(old, context.epoch, InputReason::Requested, e);
        }
        match request.action {
            RoutingAction::LeaveEdit => self.clear(InputReason::Requested, &mut e),
            RoutingAction::ReleaseTakeover => {
                if self.takeover != request.target {
                    e.code = Some(RoutingCode::NotOwner);
                    return self.finish(old, context.epoch, InputReason::Requested, e);
                }
                self.clear(InputReason::Requested, &mut e);
            }
            RoutingAction::EnterEdit | RoutingAction::Takeover => {
                if !context.foreground || !context.visible || !context.healthy {
                    e.code = Some(RoutingCode::Unavailable);
                    return self.finish(old, context.epoch, InputReason::Requested, e);
                }
                if request.action == RoutingAction::Takeover {
                    let Some(w) = self
                        .widget(request.target)
                        .filter(|w| w.keyboard && Self::eligible(*w, self.width, self.height))
                    else {
                        e.code = Some(RoutingCode::Unavailable);
                        return self.finish(old, context.epoch, InputReason::Requested, e);
                    };
                    if self.takeover != Some(w.instance) {
                        self.clear(InputReason::Requested, &mut e);
                        self.focus_to(Some(w.instance), context, &mut e);
                    }
                    self.takeover = Some(w.instance);
                } else if !self.exclusive {
                    self.clear(InputReason::Requested, &mut e);
                }
                for i in 0..256 {
                    if self.qkeys[i] && !context.physical_keys[i] {
                        self.qkeys[i] = false;
                        self.key_blocked[i] = false;
                    }
                }
                for i in 0..5 {
                    if self.qbuttons[i] && !context.physical_buttons[i] {
                        self.qbuttons[i] = false;
                        self.button_blocked[i] = false;
                    }
                }
                self.exclusive = true;
                self.armed = true;
            }
        }
        self.epoch = context.epoch;
        e.code = Some(RoutingCode::Applied);
        self.finish(old, context.epoch, InputReason::Requested, e)
    }
    fn pointer_to(&mut self, target: Option<WidgetRef>, e: &mut RouteEffects) {
        if self.pointer != target {
            if let Some(old) = self.pointer {
                self.emit(e, old, EventKind::Leave, 0);
            }
            self.pointer = target;
        }
    }
    fn deliver(&mut self, target: WidgetRef, mut record: InputRecord, e: &mut RouteEffects) -> bool {
        let Some(w) = self.widget(Some(target)) else {
            return false;
        };
        let point = match record.kind {
            3 => Some((&mut record.a, &mut record.b)),
            4 => Some((&mut record.b, &mut record.c)),
            5 => Some((&mut record.c, &mut record.d)),
            _ => None,
        };
        if let Some((x, y)) = point {
            let Some(local_x) = x.checked_sub(w.x) else {
                return false;
            };
            let Some(local_y) = y.checked_sub(w.y) else {
                return false;
            };
            *x = local_x;
            *y = local_y;
        }
        e.push(RoutedEvent {
            revision: self.revision,
            target: Some(target),
            kind: EventKind::Input,
            argument: 0,
            input: Some(record),
        });
        true
    }
    pub fn input(&mut self, record: InputRecord, context: RouteContext) -> RouteEffects {
        if context.epoch < self.epoch {
            let e = RouteEffects { pass: false, code: Some(RoutingCode::StaleEpoch), ..RouteEffects::default() };
            return self.finish(self.domains(), self.epoch, InputReason::Ready, e);
        }
        if record.validate().is_err() {
            return self.revoke(InputReason::Unavailable);
        }
        if !context.foreground {
            return self.revoke(InputReason::FocusLost);
        }
        if !context.visible {
            return self.revoke(InputReason::Hidden);
        }
        if !context.healthy {
            return self.revoke(InputReason::LeaseExpired);
        }
        if !self.exclusive && !context.cursor_available && self.domains() != Domains::default() {
            return self.revoke(InputReason::Unavailable);
        }
        if !self.configured {
            return RouteEffects::default();
        }
        if self.epoch != context.epoch {
            let e = self.revoke(InputReason::Ready);
            self.epoch = context.epoch;
            return e;
        }
        let old = self.domains();
        let mut e = RouteEffects::default();
        let down = record.flags & 1 != 0;
        let point = match record.kind {
            3 => Some((record.a, record.b)),
            4 => Some((record.b, record.c)),
            5 => Some((record.c, record.d)),
            _ => None,
        };
        if let Some((x, y)) = point {
            self.point = Some((x, y));
            let target =
                if self.exclusive || context.cursor_available { self.capture.or(self.hit(x, y)) } else { None };
            self.pointer_to(target, &mut e);
        }
        let mut delivered = true;
        match record.kind {
            1 => {
                let vk = record.a as usize;
                if self.qkeys[vk] {
                    e.pass = !self.key_blocked[vk] && !self.domains().keyboard;
                    if !down {
                        self.qkeys[vk] = false;
                        self.key_blocked[vk] = false;
                    }
                } else {
                    let target = if down { self.keys[vk].or(self.focus) } else { self.keys[vk] };
                    e.pass = !self.domains().keyboard;
                    if let Some(target) = target {
                        delivered = self.deliver(target, record, &mut e);
                        if down {
                            self.keys[vk] = Some(target);
                            self.key_blocked[vk] = !e.pass;
                        } else {
                            self.keys[vk] = None;
                            self.key_blocked[vk] = false;
                        }
                    }
                }
            }
            2 => {
                e.pass = !self.domains().keyboard;
                if let Some(target) = self.focus {
                    delivered = self.deliver(target, record, &mut e);
                }
            }
            4 => {
                let button = record.a as usize;
                if self.qbuttons[button] {
                    e.pass = !self.button_blocked[button] && !self.domains().mouse;
                    if !down {
                        self.qbuttons[button] = false;
                        self.button_blocked[button] = false;
                    }
                } else if down {
                    if let Some(target) = self.capture.or(self.pointer) {
                        self.armed = true;
                        let focus = self.widget(Some(target)).filter(|w| w.keyboard).map(|w| w.instance);
                        self.focus_to(focus, context, &mut e);
                        self.capture = Some(target);
                        self.buttons[button] = true;
                        e.pass = !self.domains().mouse;
                        self.button_blocked[button] = !e.pass;
                        delivered = self.deliver(target, record, &mut e);
                    } else if self.exclusive {
                        self.clear(InputReason::Requested, &mut e);
                        self.qbuttons[button] = true;
                        self.button_blocked[button] = true;
                        e.pass = false;
                    } else {
                        self.focus_to(None, context, &mut e);
                        e.pass = true;
                    }
                } else {
                    e.pass = !self.button_blocked[button] && !self.domains().mouse;
                    if let Some(target) = self.capture.filter(|_| self.buttons[button]) {
                        delivered = self.deliver(target, record, &mut e);
                    }
                    self.buttons[button] = false;
                    self.button_blocked[button] = false;
                    if !self.buttons.iter().any(|held| *held) {
                        self.capture = None;
                        if let Some((x, y)) = self.point {
                            let target = if self.exclusive || context.cursor_available { self.hit(x, y) } else { None };
                            self.pointer_to(target, &mut e);
                        }
                    }
                }
            }
            3 | 5 => {
                e.pass = !self.domains().mouse;
                if let Some(target) = self.capture.or(self.pointer)
                    && (self.exclusive
                        || self.armed
                        || self.widget(Some(target)).is_some_and(|w| w.mode == InputMode::Notify))
                {
                    delivered = self.deliver(target, record, &mut e);
                }
            }
            _ => {}
        }
        if !delivered {
            return self.revoke(InputReason::Unavailable);
        }
        self.finish(old, context.epoch, InputReason::Requested, e)
    }
}
