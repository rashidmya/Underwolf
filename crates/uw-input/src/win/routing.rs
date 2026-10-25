//! The UI owner serializes routing and physical/game-held accounting under one guard.
use super::*;
use crate::{
    configuration::PreparedConfig,
    hotkeys::HotkeyMatcher,
    routing::{RouteContext, RouteEffects, Router},
    text::Utf16Text,
};
use uw_pipe_proto::routing::*;
pub(super) struct Owner {
    pub core: Policy,
    pub router: Router,
    pub hotkeys: HotkeyMatcher,
    pub text: Utf16Text,
}
impl Owner {
    pub fn new() -> Self {
        Self {
            core: Policy::new(),
            router: Router::new(),
            hotkeys: HotkeyMatcher::default(),
            text: Utf16Text::default(),
        }
    }
}
impl std::ops::Deref for Owner {
    type Target = Policy;
    fn deref(&self) -> &Policy {
        &self.core
    }
}
impl std::ops::DerefMut for Owner {
    fn deref_mut(&mut self) -> &mut Policy {
        &mut self.core
    }
}
impl InputHandle {
    /// Select v2 before any lease can enable the legacy fixed chord.
    pub fn connect_routing(&self) {
        if !self.shared.routing_mode.swap(true, Ordering::AcqRel) {
            self.shared.announced_epoch.store(0, Ordering::Release);
            self.shared.submit(PolicyEvent::Disconnected);
        }
        self.shared.submit(PolicyEvent::Connected);
    }
    pub fn configure(&self, config: PreparedConfig) -> Result<(), Full> {
        let result = self.shared.configurations.try_push(config);
        if result.is_err() {
            self.shared.revoke(InputReason::Overflow);
        }
        window::wake(&self.shared);
        result
    }
    pub fn route_request(&self, request: RoutingRequest) -> Result<(), Full> {
        let release_now = request.action == RoutingAction::LeaveEdit
            && RoutingMessage::Request(request).validate().is_ok()
            && (self.shared.state_sequence.is_current(request.state_seq)
                || !self.shared.visible_intent.load(Ordering::Acquire)
                    && self.shared.state_sequence.not_future(request.state_seq))
            && self
                .shared
                .policy
                .try_lock()
                .is_ok_and(|p| p.epoch() == request.epoch && p.router.revision() == request.revision);
        let result = if let Ok(mut queue) = self.shared.route_requests.try_lock()
            && queue.len() < 32
        {
            queue.push_back(request);
            if release_now {
                self.shared.revoke(InputReason::Requested);
            }
            Ok(())
        } else {
            self.shared.revoke(InputReason::Overflow);
            Err(Full)
        };
        window::wake(&self.shared);
        result
    }
}
impl Shared {
    pub(super) fn observe_client_geometry(&self) {
        if !self.routing_mode.load(Ordering::Acquire) || self.hwnd() == 0 {
            return;
        }
        let mut rect = windows_sys::Win32::Foundation::RECT::default();
        // SAFETY: owner-thread query of the current target, before taking the policy guard.
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd() as HWND, &mut rect) } == 0 {
            self.revoke(InputReason::Unavailable);
            return;
        }
        let effects = if let Ok(mut policy) = self.policy.try_lock() {
            let effects = policy
                .router
                .set_client_bounds((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32);
            if effects.is_some() {
                policy.text.reset();
            }
            effects
        } else {
            self.revoke(InputReason::Unavailable);
            return;
        };
        if let Some(effects) = effects {
            self.permit.store(false, Ordering::Release);
            native::revoke(InputReason::Requested);
            self.queue_routed(&effects, None, false);
            lowlevel::reset_text();
            if !self.apply_domains(effects.domains) {
                self.revoke(InputReason::Unavailable);
            }
            self.report(InputReason::Requested, None);
        }
    }
    pub(super) fn desired_domains(&self) -> Domains {
        self.policy.try_lock().map(|p| p.domains()).unwrap_or_default()
    }
    pub(super) fn capture_domain(&self, mouse: bool) -> bool {
        if !self.capture() {
            return false;
        }
        if !self.routing_mode.load(Ordering::Acquire) {
            return true;
        }
        let (epoch, domains, _) = native::ownership();
        if epoch != self.epoch.load(Ordering::Acquire) {
            return false;
        }
        if mouse { domains.mouse } else { domains.keyboard }
    }
    pub(super) fn publish_route_permission(&self) {
        if self.updating_domains.load(Ordering::Acquire) {
            return;
        }
        if !self.routing_mode.load(Ordering::Acquire) {
            return;
        }
        if !self.visible_intent.load(Ordering::Acquire) {
            self.permit.store(false, Ordering::Release);
            native::revoke(InputReason::Requested);
            return;
        }
        if let Ok(p) = self.policy.try_lock() {
            if p.active() && self.permit.load(Ordering::Acquire) && self.release.load(Ordering::Acquire) == 0 {
                if p.router.exclusive() {
                    cursor::exclusive();
                }
                if !native::publish_ownership(p.epoch(), p.domains(), p.router.exclusive()) {
                    if self.state_sequence.publication_allowed() {
                        self.revoke(InputReason::Unavailable);
                    } else {
                        self.permit.store(false, Ordering::Release);
                        native::revoke(InputReason::Requested);
                    }
                }
            } else {
                native::revoke(InputReason::Unavailable);
            }
        } else {
            native::revoke(InputReason::Unavailable);
        }
    }
    fn context(&self, p: &Owner, cursor: bool, at: u64) -> RouteContext {
        let healthy = self.lease.try_lock().is_ok_and(|lease| lease.healthy_in(p.epoch(), at));
        let mut context = p.route_context(healthy && native::ready(), cursor);
        context.visible &= self.visible_intent.load(Ordering::Acquire);
        context
    }
    pub(super) fn report_routing(&self, state: InputState) {
        if let Ok(p) = self.policy.try_lock() {
            let (epoch, published, exclusive) = native::ownership();
            let active = state.active && epoch == state.epoch;
            let domains = if active { published } else { Domains::default() };
            let mut report = RoutingState {
                epoch: state.epoch,
                revision: p.router.revision(),
                state_seq: state.state_seq.filter(|sequence| *sequence != 0),
                width: state.width,
                height: state.height,
                pointer: p.router.pointer(),
                focus: p.router.focus(),
                takeover: if active && exclusive { p.router.takeover() } else { None },
                exclusive: active && exclusive,
                domains,
                reason: state.reason,
            };
            if let Ok(mut latest) = self.routing_state.try_lock() {
                if let Some(previous) = latest.as_ref()
                    && previous.epoch == report.epoch
                    && report.domains == Domains::default()
                    && !matches!(previous.reason, InputReason::Ready | InputReason::Requested | InputReason::Hotkey)
                    && matches!(report.reason, InputReason::Ready | InputReason::Requested)
                {
                    report.reason = previous.reason;
                }
                *latest = Some(report);
                return;
            }
        }
        self.revoke(InputReason::Overflow);
    }
    pub(super) fn queue_routed(&self, effects: &RouteEffects, barrier: Option<RoutingResult>, request: bool) {
        let mut records =
            [RoutedEvent { revision: 1, target: None, kind: EventKind::Hotkey, argument: 1, input: None }; 35];
        let mut count = 0;
        for event in effects.events.iter().flatten() {
            records[count] = *event;
            count += 1;
        }
        if count == 0 && barrier.is_none() {
            return;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        if epoch != self.announced_epoch.load(Ordering::Acquire) {
            // A safety failure invalidated the old delivery stream. Report failure through
            // independent control space; the new epoch clears receiver-held state.
            if let Some(mut result) = barrier {
                if result.code == RoutingCode::Applied {
                    result.code = RoutingCode::Superseded;
                }
                result.through_seq = 0;
                self.emit(if request { RuntimeEvent::RoutingResult(result) } else { RuntimeEvent::Configured(result) });
            }
            return;
        }
        let result = if request && let Some(result) = barrier {
            self.routed_outbox.try_request_group(epoch, &records[..count], result)
        } else {
            self.routed_outbox.try_group(epoch, &records[..count], barrier)
        };
        if matches!(result, crate::delivery::PushResult::Revoke | crate::delivery::PushResult::Stale) {
            self.revoke(InputReason::Overflow);
        }
    }
    /// Native effects run after the policy guard has been released, including reentrant game cleanup.
    fn apply_domains(&self, domains: Domains) -> bool {
        let at = now();
        if self.updating_domains.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            self.revoke(InputReason::Unavailable);
            return false;
        }
        struct Updating<'a>(&'a AtomicBool);
        impl Drop for Updating<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let updating = Updating(&self.updating_domains);
        let (old, effects, active) = {
            let Ok(mut p) = self.policy.try_lock() else {
                self.revoke(InputReason::Unavailable);
                return false;
            };
            let context = self.context(&p, true, at);
            let old = p.domains();
            let effects = p.own_domains(domains, context.healthy);
            if let Some(Transition::Release { reason, .. }) = effects.transition
                && reason != InputReason::Requested
            {
                p.router.revoke(reason);
                p.text.reset();
            }
            self.epoch.store(p.epoch(), Ordering::Release);
            (old, effects, p.active())
        };
        if old == domains && effects.transition.is_none() {
            drop(updating);
            self.publish_route_permission();
            return domains == Domains::default() || self.capture();
        }
        if active && effects.transition.is_none() {
            let common = Domains { keyboard: old.keyboard && domains.keyboard, mouse: old.mouse && domains.mouse };
            if !native::publish_ownership(self.epoch.load(Ordering::Acquire), common, false) {
                self.revoke(InputReason::Unavailable);
                return false;
            }
        } else {
            self.permit.store(false, Ordering::Release);
            native::revoke(InputReason::Requested);
        }
        if effects.transition.is_some() {
            self.effects(effects);
        } else if active && old != domains {
            let registration = registration::prepare_for(domains);
            self.setting_up.store(domains.mouse, Ordering::Release);
            let cursor = if !old.mouse && domains.mouse { cursor::prepare() } else { Some(0) };
            let success = registration.is_some()
                && cursor.is_some()
                && window::release_held(self, &effects.releases)
                && self.release.load(Ordering::Acquire) == 0
                && !self.session_failed.load(Ordering::Acquire);
            let cursor_ready = if !old.mouse && domains.mouse {
                cursor.is_some_and(|token| cursor::activate(token, success))
            } else if old.mouse && !domains.mouse {
                bypass(|| cursor::restore(true, true)) && success
            } else {
                success
            };
            let success = registration.is_some_and(|token| registration::activate(token, cursor_ready));
            self.setting_up.store(false, Ordering::Release);
            if !success {
                self.revoke(InputReason::SetupFailed);
                return false;
            }
        }
        if let Ok(p) = self.policy.try_lock() {
            self.permit.store(
                p.active() && self.release.load(Ordering::Acquire) == 0 && !self.session_failed.load(Ordering::Acquire),
                Ordering::Release,
            );
        }
        drop(updating);
        self.publish_route_permission();
        directinput::synchronize(true);
        domains == Domains::default() || self.capture()
    }
    pub(super) fn service_routing(&self) {
        if !self.routing_mode.load(Ordering::Acquire) {
            return;
        }
        self.observe_client_geometry();
        while let Some(config) = self.configurations.pop() {
            let at = now();
            let c = config.config();
            let mut result = RoutingResult {
                epoch: c.epoch,
                revision: c.revision,
                request_id: c.request_id,
                code: RoutingCode::Applied,
                through_seq: 0,
            };
            let (effects, changed_focus) = {
                let Ok(mut p) = self.policy.try_lock() else {
                    self.revoke(InputReason::Overflow);
                    return;
                };
                if !self.context(&p, true, at).healthy && p.active() {
                    self.revoke(InputReason::LeaseExpired);
                }
                if c.epoch != p.epoch() {
                    result.code = RoutingCode::StaleEpoch;
                    (RouteEffects::default(), false)
                } else {
                    let focus = p.router.focus();
                    let effects = p.router.configure(&config);
                    result.code = effects.code.unwrap_or(RoutingCode::Invalid);
                    if result.code == RoutingCode::Applied {
                        let physical = p.physical_keys();
                        p.hotkeys.replace(&config, &physical);
                    }
                    let changed = focus != p.router.focus();
                    if changed {
                        p.text.reset();
                    }
                    (effects, changed)
                }
            };
            if changed_focus {
                lowlevel::reset_text();
            }
            if result.code == RoutingCode::Applied && !self.apply_domains(effects.domains) {
                result.code = RoutingCode::SetupFailed;
            }
            self.queue_routed(&effects, Some(result), false);
            self.report(InputReason::Requested, None);
            native::request_coverage();
        }
        loop {
            let next = self.route_requests.try_lock().ok().and_then(|mut q| q.pop_front());
            let Some(request) = next else { break };
            let cursor = cursor::available();
            let at = now();
            let (effects, changed_focus, stale_state) = {
                let Ok(mut p) = self.policy.try_lock() else {
                    self.revoke(InputReason::Overflow);
                    return;
                };
                let context = self.context(&p, cursor, at);
                let focus = p.router.focus();
                let stale_state = request.epoch == context.epoch
                    && RoutingMessage::Request(request).validate().is_ok()
                    && !self.state_sequence.is_current(request.state_seq)
                    && !(request.action == RoutingAction::LeaveEdit
                        && !self.visible_intent.load(Ordering::Acquire)
                        && self.state_sequence.not_future(request.state_seq));
                let effects = if stale_state {
                    let mut effects = RouteEffects::default();
                    effects.domains = p.router.domains();
                    effects.exclusive = p.router.exclusive();
                    effects.code = Some(RoutingCode::Superseded);
                    effects
                } else {
                    p.router.request(request, context)
                };
                let changed = focus != p.router.focus();
                if changed {
                    p.text.reset();
                }
                (effects, changed, stale_state)
            };
            if changed_focus {
                lowlevel::reset_text();
            }
            let mut code = effects.code.unwrap_or(RoutingCode::Invalid);
            let release = request.action == RoutingAction::LeaveEdit;
            let _applying = self.state_sequence.begin(if stale_state || release { None } else { request.state_seq });
            let success = stale_state || self.apply_domains(effects.domains);
            if code == RoutingCode::Applied && !success {
                code = RoutingCode::SetupFailed;
            }
            let mut final_effects = effects;
            if !stale_state && !release && !self.state_sequence.is_current(request.state_seq) {
                code = RoutingCode::Superseded;
                self.queue_routed(&final_effects, None, false);
                let cancellation = self.policy.try_lock().ok().map(|mut p| p.router.revoke(InputReason::Requested));
                if let Some(cancellation) = cancellation {
                    self.apply_domains(cancellation.domains);
                    final_effects = cancellation;
                } else {
                    self.revoke(InputReason::Unavailable);
                }
            }
            self.queue_routed(
                &final_effects,
                Some(RoutingResult {
                    epoch: request.epoch,
                    revision: request.revision,
                    request_id: request.request_id,
                    code,
                    through_seq: 0,
                }),
                true,
            );
            if code == RoutingCode::Applied
                && self.release.load(Ordering::Acquire) == 0
                && self.epoch.load(Ordering::Acquire) == request.epoch
                && let Some(sequence) = request.state_seq
            {
                self.emit(RuntimeEvent::Applied(sequence));
            }
            self.report(InputReason::Requested, if code == RoutingCode::Applied { request.state_seq } else { None });
        }
    }
    pub(super) fn ingress(&self, record: InputRecord, raw: bool) -> Option<Effects> {
        self.ingress_source(record, raw, false).map(|(effects, _)| effects)
    }
    pub(super) fn ingress_lowlevel(&self, record: InputRecord) -> Option<(Effects, bool)> {
        self.ingress_source(record, false, true)
    }
    fn ingress_source(&self, record: InputRecord, raw: bool, low: bool) -> Option<(Effects, bool)> {
        if !self.routing_mode.load(Ordering::Acquire) {
            return self
                .event(if raw { PolicyEvent::Observed(record) } else { PolicyEvent::Input(record) })
                .map(|effects| (effects, false));
        }
        if record.validate().is_err() {
            self.revoke(InputReason::Unavailable);
            return None;
        }
        let mouse = record.kind != 1 && record.kind != 2;
        let raw_owner = self.raw_source.load(Ordering::Acquire) & (if mouse { 1 } else { 2 }) != 0;
        let fallback = lowlevel::active();
        if record.kind != 2 && (if fallback { !low } else { raw != raw_owner }) {
            let pass = if raw || fallback {
                !(self.capture_domain(mouse) || record.kind == 1 && self.blocks_key(record.a))
            } else {
                false
            };
            return Some((Effects { pass, ..Effects::default() }, false));
        }
        let cursor = cursor::available();
        let at = now();
        let (mut routed, consumed, focus, keyboard, changed_focus) = {
            let Ok(mut p) = self.policy.try_lock() else {
                self.revoke(InputReason::Overflow);
                return None;
            };
            let context = self.context(&p, cursor, at);
            let focus = p.router.focus();
            let mut physical = p.physical_keys();
            if record.kind == 1 {
                physical[record.a as usize] = record.flags & 1 != 0;
            }
            let hit = if context.healthy && context.foreground && context.visible {
                p.hotkeys.observe(record, &physical)
            } else {
                p.hotkeys.reseed(&physical);
                None
            };
            let consumed = p.hotkeys.consumes_last();
            let mut routed = if let Some(hit) = hit {
                let action = match hit.action {
                    HotkeyAction::ToggleEdit => {
                        Some(if p.router.exclusive() { RoutingAction::LeaveEdit } else { RoutingAction::EnterEdit })
                    }
                    HotkeyAction::Takeover => Some(RoutingAction::Takeover),
                    HotkeyAction::ReleaseTakeover => Some(RoutingAction::ReleaseTakeover),
                    HotkeyAction::Notify => None,
                };
                let mut effects = if let Some(action) = action {
                    let request = RoutingRequest {
                        epoch: context.epoch,
                        revision: p.router.revision(),
                        request_id: hit.binding_id,
                        target: hit.target,
                        action,
                        state_seq: None,
                    };
                    p.router.request(request, context)
                } else if consumed {
                    let mut effects = RouteEffects::default();
                    effects.domains = p.router.domains();
                    effects.exclusive = p.router.exclusive();
                    effects
                } else {
                    p.router.input(record, context)
                };
                if let Some(slot) = effects.events.iter_mut().find(|e| e.is_none()) {
                    *slot = Some(RoutedEvent {
                        revision: p.router.revision(),
                        target: None,
                        kind: EventKind::Hotkey,
                        argument: hit.binding_id,
                        input: None,
                    });
                } else {
                    self.revoke(InputReason::Overflow);
                }
                effects
            } else if consumed {
                {
                    let mut e = RouteEffects::default();
                    e.domains = p.router.domains();
                    e.exclusive = p.router.exclusive();
                    e
                }
            } else {
                p.router.input(record, context)
            };
            if consumed {
                routed.pass = false;
            }
            if focus != p.router.focus() {
                p.text.reset();
            }
            (routed, consumed, p.router.focus(), p.router.domains().keyboard, focus != p.router.focus())
        };
        if changed_focus {
            lowlevel::reset_text();
        }
        if !self.apply_domains(routed.domains)
            && let Ok(mut p) = self.policy.try_lock()
        {
            routed = p.router.revoke(InputReason::SetupFailed);
        }
        let at = now();
        let mut effects = {
            let Ok(mut p) = self.policy.try_lock() else {
                self.revoke(InputReason::Overflow);
                return None;
            };
            let healthy = self.context(&p, cursor, at).healthy;
            let mut e =
                p.event_with_lease(PolicyEvent::RoutedInput { record, pass: routed.pass && !consumed }, healthy);
            e.host = [None; 2];
            // Signal TranslateMessage only for an actual routed, owned key-down.
            if record.kind == 1
                && record.flags & 1 != 0
                && (keyboard || raw || low)
                && focus.is_some()
                && routed.events.iter().flatten().any(|e| e.kind == EventKind::Input && e.input == Some(record))
            {
                e.host[0] = Some(record);
            }
            e
        };
        effects.pass &= routed.pass && !consumed;
        self.queue_routed(&routed, None, false);
        self.report(InputReason::Ready, None);
        Some((effects, consumed))
    }
    pub(super) fn routed_char(&self, scalar: u32, utf16: bool) -> bool {
        let chars = {
            let Ok(mut p) = self.policy.try_lock() else {
                self.revoke(InputReason::Overflow);
                return true;
            };
            if utf16 { p.text.push(scalar as u16) } else { [char::from_u32(scalar), None] }
        };
        let mut pass = true;
        for ch in chars.into_iter().flatten() {
            if let Some(e) = self.ingress(InputRecord::text(ch, 0), false) {
                pass &= e.pass;
            }
        }
        pass
    }
}
