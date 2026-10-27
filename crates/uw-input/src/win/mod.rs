//! Windows ownership shell. Capture remains unavailable until all required adapters install.
pub mod abi;
mod cursor;
pub mod directinput;
pub mod hooks;
mod keyboard;
mod lowlevel;
pub mod native;
mod raw;
mod registration;
mod routing;
mod window;
use crate::delivery::{Lease, SentProgress};
use crate::{
    delivery::Outbox,
    policy::{Effects, Policy, PolicyEvent, Transition},
    requests::{Full, RequestQueue},
};
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use uw_pipe_proto::input::*;
thread_local! { static BYPASS:Cell<bool>=const{Cell::new(false)}; }
fn bypassed() -> bool {
    BYPASS.with(Cell::get)
}
fn bypass<T>(f: impl FnOnce() -> T) -> T {
    struct Guard(bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            BYPASS.with(|b| b.set(self.0))
        }
    }
    let _guard = Guard(BYPASS.with(|b| b.replace(true)));
    f()
}
fn boundary<T>(fallback: T, operation: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(value) => value,
        Err(_) => {
            if let Some(shared) = SHARED.get() {
                shared.revoke(InputReason::Unavailable);
            }
            fallback
        }
    }
}
fn now() -> u64 {
    // SAFETY: monotonic system tick query has no arguments.
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}
use windows_sys::Win32::Foundation::HWND;
#[derive(Debug)]
pub struct InstallError(pub String);
#[derive(Debug)]
pub enum RuntimeEvent {
    Disconnect,
    State(InputState),
    Result(CaptureResult),
    Applied(u64),
    Batch(InputBatch),
    RoutingState(uw_pipe_proto::routing::RoutingState),
    Configured(uw_pipe_proto::routing::RoutingResult),
    RoutedBatch(uw_pipe_proto::routing::RoutedBatch),
    RoutingResult(uw_pipe_proto::routing::RoutingResult),
    NativeCoverage(uw_pipe_proto::routing::NativeCoverage),
}
pub struct InputRuntime {
    shared: Arc<Shared>,
}
#[derive(Clone)]
pub struct InputHandle {
    shared: Arc<Shared>,
}
pub(super) struct Shared {
    state_sequence: crate::requests::StateSequence,
    visible_intent: AtomicBool,
    hwnd: AtomicUsize,
    thread: AtomicU32,
    message: u32,
    policy: Mutex<routing::Owner>,
    suppressed_keys: [AtomicU64; 4],
    routing_mode: AtomicBool,
    configurations: crate::requests::ConfigurationQueue,
    route_requests: Mutex<VecDeque<uw_pipe_proto::routing::RoutingRequest>>,
    routed_outbox: crate::delivery::RoutedOutbox,
    routing_state: Mutex<Option<uw_pipe_proto::routing::RoutingState>>,
    raw_source: AtomicUsize,
    requests: RequestQueue,
    events: Mutex<VecDeque<RuntimeEvent>>,
    state: Mutex<Option<InputState>>,
    session_failed: AtomicBool,
    permit: AtomicBool,
    setting_up: AtomicBool,
    updating_domains: AtomicBool,
    epoch: AtomicU64,
    release: AtomicUsize,
    restore_epoch: AtomicU64,
    restore_intent: AtomicBool,
    pub outbox: Outbox,
    lease: Mutex<Lease>,
    progress: Mutex<SentProgress>,
    announced_epoch: AtomicU64,
    installed: AtomicBool,
    owner_events: Mutex<VecDeque<(Option<u64>, PolicyEvent)>>,
}
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
impl InputRuntime {
    // HWND is an opaque OS handle, validated before use; we never dereference it.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn install(hwnd: HWND) -> Result<Self, InstallError> {
        if !crate::cursor::CursorJournal::lock_free() {
            return Err(InstallError("128-bit hardware atomics required for cursor intent".into()));
        }
        use windows_sys::Win32::{
            System::Threading::GetCurrentProcessId, UI::WindowsAndMessaging::GetWindowThreadProcessId,
        };
        let mut pid = 0;
        // SAFETY: queries the window; pid points to writable storage.
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // SAFETY: GetCurrentProcessId has no preconditions.
        if thread == 0 || pid != unsafe { GetCurrentProcessId() } {
            return Err(InstallError("window is not in this process".into()));
        }
        let message = window::message()?;
        let shared = Arc::new(Shared {
            state_sequence: crate::requests::StateSequence::new(),
            visible_intent: AtomicBool::new(false),
            hwnd: AtomicUsize::new(hwnd as usize),
            thread: AtomicU32::new(thread),
            message,
            policy: Mutex::new(routing::Owner::new()),
            suppressed_keys: [const { AtomicU64::new(0) }; 4],
            routing_mode: AtomicBool::new(false),
            configurations: crate::requests::ConfigurationQueue::new(),
            route_requests: Mutex::new(VecDeque::with_capacity(32)),
            routed_outbox: crate::delivery::RoutedOutbox::new(),
            routing_state: Mutex::new(None),
            raw_source: AtomicUsize::new(0),
            requests: RequestQueue::new(),
            events: Mutex::new(VecDeque::with_capacity(32)),
            state: Mutex::new(None),
            session_failed: AtomicBool::new(false),
            permit: AtomicBool::new(false),
            setting_up: AtomicBool::new(false),
            updating_domains: AtomicBool::new(false),
            epoch: AtomicU64::new(1),
            release: AtomicUsize::new(0),
            restore_epoch: AtomicU64::new(0),
            restore_intent: AtomicBool::new(false),
            outbox: Outbox::new(),
            lease: Mutex::new(Lease::new(1)),
            progress: Mutex::new(SentProgress::default()),
            announced_epoch: AtomicU64::new(0),
            installed: AtomicBool::new(false),
            owner_events: Mutex::new(VecDeque::with_capacity(32)),
        });
        SHARED.set(shared.clone()).map_err(|_| InstallError("input runtime already installed".into()))?;
        native::observe_modules(&shared)?;
        window::install(&shared)?;
        let watchdog = shared.clone();
        std::thread::Builder::new()
            .name("uw-input-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    native::poll(&watchdog);
                    if watchdog.release.load(Ordering::Acquire) != 0 {
                        window::wake(&watchdog);
                    }
                    window::rediscover(&watchdog);
                    if watchdog.restore_epoch.load(Ordering::Acquire) != 0 {
                        window::wake(&watchdog);
                    }
                    if watchdog.permit.load(Ordering::Acquire) {
                        let healthy = watchdog.lease.try_lock().is_ok_and(|l| l.healthy(now()));
                        if !healthy {
                            watchdog.revoke(InputReason::LeaseExpired);
                        }
                        if watchdog.outbox.revoked() || watchdog.routed_outbox.revoked() {
                            watchdog.revoke(InputReason::Overflow);
                        }
                        watchdog.capture();
                    }
                }
            })
            .map_err(|e| InstallError(e.to_string()))?;
        Ok(Self { shared })
    }
    pub fn handle(&self) -> InputHandle {
        InputHandle { shared: self.shared.clone() }
    }
}
impl InputHandle {
    pub fn connect(&self) {
        self.shared.submit(PolicyEvent::Connected);
    }
    pub fn disconnect(&self) {
        self.shared.revoke(InputReason::Disconnected);
        self.shared.submit(PolicyEvent::Disconnected);
    }
    pub fn observe_state_sequence(&self, sequence: u64) {
        if self.shared.state_sequence.observe(sequence) {
            native::revoke(InputReason::Requested);
        }
    }
    pub fn defer_hidden(&self) {
        self.shared.visible_intent.store(false, Ordering::Release);
        self.shared.permit.store(false, Ordering::Release);
        native::revoke(InputReason::Requested);
    }
    pub fn set_visible_state(&self, on: bool, state_seq: u64) {
        self.shared.visible_intent.store(on, Ordering::Release);
        self.observe_state_sequence(state_seq);
        self.shared.submit(PolicyEvent::VisibleState { on, state_seq });
        if !on {
            self.shared.revoke(InputReason::Hidden);
        }
    }
    pub fn set_visible(&self, on: bool) {
        self.shared.visible_intent.store(on, Ordering::Release);
        self.shared.submit(PolicyEvent::Visible(on));
        if !on {
            self.shared.revoke(InputReason::Hidden);
        }
    }
    pub fn acknowledge(&self, ack: InputAck) {
        let healthy = self.shared.lease.try_lock().is_ok_and(|mut l| l.ack(ack, now()));
        if healthy {
            self.shared.submit_epoch(Some(ack.epoch), PolicyEvent::Healthy(true));
        }
    }
    pub fn challenge(&self) -> Option<InputChallenge> {
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        // Only the draining consumer can announce an epoch. The UI may advance it
        // between drain() and challenge(), before its State event is queued.
        if self.shared.announced_epoch.load(Ordering::Acquire) != epoch {
            return None;
        }
        let mut lease = self.shared.lease.try_lock().ok()?;
        lease.reset(epoch);
        lease.challenge(now(), self.shared.progress.lock().ok()?.through(epoch))
    }
    pub fn apply_capture(&self, request: CaptureRequest, visible: bool, state_seq: u64) {
        self.shared.submit(PolicyEvent::CaptureState { request, visible, state_seq });
        if !visible && request.epoch == self.shared.epoch.load(Ordering::Acquire) {
            self.shared.revoke(InputReason::Hidden);
        }
    }
    pub fn request(&self, r: CaptureRequest) -> Result<(), Full> {
        let result = self.shared.requests.try_push(r);
        if result.is_err() {
            self.revoke(InputReason::Overflow)
        }
        window::wake(&self.shared);
        result
    }
    pub fn revoke(&self, reason: InputReason) {
        self.shared.revoke(reason);
    }
    pub fn drain(&self) -> Vec<RuntimeEvent> {
        if self.shared.session_failed.swap(false, Ordering::AcqRel) {
            if let Ok(mut events) = self.shared.events.lock() {
                events.clear();
            }
            return vec![RuntimeEvent::Disconnect];
        }
        let routing = self.shared.routing_mode.load(Ordering::Acquire);
        let mut events = self.shared.events.lock().map(|mut q| q.drain(..).collect::<Vec<_>>()).unwrap_or_default();
        if let Ok(mut state) = self.shared.state.lock()
            && let Some(state) = state.take()
        {
            events.push(RuntimeEvent::State(state));
        }
        if let Ok(mut state) = self.shared.routing_state.lock()
            && let Some(state) = state.take()
        {
            events.insert(0, RuntimeEvent::RoutingState(state));
        }
        for event in &events {
            let epoch = match event {
                RuntimeEvent::State(s) if !routing => s.epoch,
                RuntimeEvent::RoutingState(s) if routing => s.epoch,
                _ => continue,
            };
            if let Ok(mut progress) = self.shared.progress.lock() {
                progress.announce(epoch);
            }
            let previous = self.shared.announced_epoch.swap(epoch, Ordering::AcqRel);
            if routing && previous != epoch {
                native::request_coverage();
            }
        }
        if routing {
            let announced = self.shared.announced_epoch.load(Ordering::Acquire);
            events.retain(|event| {
                if let RuntimeEvent::NativeCoverage(coverage) = event {
                    if coverage.epoch > announced {
                        native::request_coverage();
                    }
                    return coverage.epoch == announced;
                }
                true
            });
        }
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if self.shared.announced_epoch.load(Ordering::Acquire) != epoch {
            return events;
        }
        if self.shared.routing_mode.load(Ordering::Acquire) {
            self.shared.routed_outbox.reset(epoch);
            if let Some(output) = self.shared.routed_outbox.take(epoch) {
                match output {
                    crate::delivery::RoutedOutput::Events(b) => {
                        if let Ok(mut progress) = self.shared.progress.lock() {
                            progress.sent(b.epoch, b.first_seq + b.events.len() as u64 - 1);
                        }
                        events.push(RuntimeEvent::RoutedBatch(b));
                    }
                    crate::delivery::RoutedOutput::Configured(r) => events.push(RuntimeEvent::Configured(r)),
                    crate::delivery::RoutedOutput::Result(r) => events.push(RuntimeEvent::RoutingResult(r)),
                }
            }
            return events;
        }
        self.shared.outbox.reset(epoch);
        if let Some(b) = self.shared.outbox.take_batch(epoch) {
            if let Ok(mut progress) = self.shared.progress.lock() {
                progress.sent(b.epoch, b.first_seq + b.records.len() as u64 - 1);
            }
            events.push(RuntimeEvent::Batch(b))
        }
        events
    }
    pub fn capture_permitted(&self) -> bool {
        self.shared.permit.load(Ordering::Acquire)
            && self.shared.release.load(Ordering::Acquire) == 0
            && (!self.shared.routing_mode.load(Ordering::Acquire) || native::ownership().1.bits() != 0)
    }
}
impl Shared {
    fn hwnd(&self) -> usize {
        self.hwnd.load(Ordering::Acquire)
    }
    fn thread(&self) -> u32 {
        self.thread.load(Ordering::Acquire)
    }

    fn revoke(&self, reason: InputReason) {
        if !matches!(reason, InputReason::Requested | InputReason::Hotkey | InputReason::Ready) {
            lowlevel::invalidate();
        }
        if reason == InputReason::Overflow {
            self.session_failed.store(true, Ordering::Release);
        }
        self.permit.store(false, Ordering::Release);
        self.release.store(reason as usize + 1, Ordering::Release);
        native::revoke(reason);
    }
    fn capture(&self) -> bool {
        if !self.permit.load(Ordering::Acquire) || self.release.load(Ordering::Acquire) != 0 {
            return false;
        }
        // SAFETY: foreground query has no pointer preconditions.
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as usize != self.hwnd() {
            self.revoke(InputReason::FocusLost);
            return false;
        }
        true
    }
    fn blocks_key(&self, vk: i32) -> bool {
        let mouse = matches!(vk, 1 | 2 | 4 | 5 | 6);
        if self.capture_domain(mouse) {
            return true;
        }
        self.suppressed_key(vk)
    }
    fn suppressed_key(&self, vk: i32) -> bool {
        (0..256).contains(&vk)
            && self.suppressed_keys[vk as usize / 64].load(Ordering::Acquire) & (1u64 << (vk as usize % 64)) != 0
    }
    // The owner publishes quarantine under its policy guard, before releasing
    // domains or invoking restoration callbacks. Foreign polling/raw readers
    // must not contend with that guard or revoke a healthy notification session.
    fn publish_suppression(&self, policy: &Policy) {
        for (word, snapshot) in self.suppressed_keys.iter().enumerate() {
            let bits = (0..64)
                .fold(0u64, |bits, offset| bits | ((policy.key_suppressed(word * 64 + offset) as u64) << offset));
            snapshot.store(bits, Ordering::Release);
        }
    }
    fn emit(&self, event: RuntimeEvent) {
        if let Ok(mut q) = self.events.try_lock()
            && q.len() < 32
        {
            q.push_back(event);
            return;
        }
        self.revoke(InputReason::Overflow);
    }
    fn submit(&self, event: PolicyEvent) {
        self.submit_epoch(None, event)
    }
    fn submit_epoch(&self, epoch: Option<u64>, event: PolicyEvent) {
        if let Ok(mut q) = self.owner_events.try_lock()
            && q.len() < 32
        {
            q.push_back((epoch, event));
            window::wake(self);
            return;
        }
        self.revoke(InputReason::Overflow);
    }
    fn event(&self, event: PolicyEvent) -> Option<Effects> {
        let at = now();
        let Ok(mut p) = self.policy.try_lock() else {
            self.revoke(InputReason::Overflow);
            return None;
        };
        if self.routing_mode.load(Ordering::Acquire) {
            p.use_configured_hotkeys();
        }
        let healthy = self.lease.try_lock().is_ok_and(|lease| lease.healthy_in(p.epoch(), at));
        let effects = p.event_with_lease(event, healthy);
        self.publish_suppression(&p);
        let epoch = p.epoch();
        let mut routed_release = None;
        if self.routing_mode.load(Ordering::Acquire) {
            let reason = match event {
                PolicyEvent::Disconnected => Some(InputReason::Disconnected),
                PolicyEvent::Destroyed => Some(InputReason::WindowDestroyed),
                PolicyEvent::Foreground(false) => Some(InputReason::FocusLost),
                PolicyEvent::Visible(false) | PolicyEvent::VisibleState { on: false, .. } => Some(InputReason::Hidden),
                PolicyEvent::Failure(r) => Some(r),
                PolicyEvent::Healthy(false) => Some(InputReason::LeaseExpired),
                _ => None,
            };
            if let Some(reason) = reason {
                let routed = p.router.revoke(reason);
                if reason == InputReason::Requested {
                    routed_release = Some(routed);
                }
                p.text.reset();
                let physical = p.core.physical_keys();
                p.hotkeys.reseed(&physical);
            }
            p.router.advance_epoch(epoch);
        }
        self.epoch.store(epoch, Ordering::Release);
        self.permit.store(
            p.active() && self.release.load(Ordering::Acquire) == 0 && !self.session_failed.load(Ordering::Acquire),
            Ordering::Release,
        );
        drop(p);
        if let Some(routed) = routed_release {
            self.queue_routed(&routed, None, false);
        }
        self.publish_route_permission();
        Some(effects)
    }
    fn report(&self, reason: InputReason, state_seq: Option<u64>) {
        let mut rect = windows_sys::Win32::Foundation::RECT::default();
        // SAFETY: target belongs to this process; rect is writable.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd() as HWND, &mut rect);
        }
        let mut state = InputState {
            epoch: self.epoch.load(Ordering::Acquire),
            active: self.capture(),
            reason,
            state_seq,
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
        };
        if self.routing_mode.load(Ordering::Acquire) {
            self.report_routing(state);
            return;
        }
        if let Ok(mut latest) = self.state.try_lock() {
            if let Some(previous) = latest.as_ref()
                && previous.epoch == state.epoch
                && !state.active
                && !matches!(previous.reason, InputReason::Ready | InputReason::Requested | InputReason::Hotkey)
                && matches!(state.reason, InputReason::Ready | InputReason::Requested)
            {
                state.reason = previous.reason;
            }
            *latest = Some(state);
        } else {
            self.revoke(InputReason::Overflow);
        }
    }
    fn effects(&self, e: Effects) {
        let epoch = self.epoch.load(Ordering::Acquire);
        for r in e.host.into_iter().flatten().filter(|_| !self.routing_mode.load(Ordering::Acquire)) {
            if matches!(
                self.outbox.try_push(epoch, r),
                crate::delivery::PushResult::Revoke | crate::delivery::PushResult::Stale
            ) {
                self.revoke(InputReason::Overflow)
            }
        }
        if let Some(seq) = e.applied {
            self.emit(RuntimeEvent::Applied(seq));
            self.report(InputReason::Requested, Some(seq));
        }
        if let Some(result) = e.result {
            self.emit(RuntimeEvent::Result(result));
            if result.code == CaptureCode::Applied {
                self.emit(RuntimeEvent::Applied(result.state_seq));
                self.report(InputReason::Requested, Some(result.state_seq));
            }
        }
        if let Some(Transition::Enter { epoch, reason }) = e.transition {
            let domains = self.desired_domains();
            let token = registration::prepare_for(domains);
            let cursor_token = token.and_then(|_| if domains.mouse { cursor::prepare() } else { Some(0) });
            self.setting_up.store(domains.mouse && cursor_token.is_some(), Ordering::Release);
            // Both transactions span arbitrary game WndProc cleanup callbacks.
            let setup = cursor_token.is_some()
                && window::release_held(self, &e.releases)
                && self.release.load(Ordering::Acquire) == 0
                && !self.session_failed.load(Ordering::Acquire);
            let cursor_ready =
                cursor_token.is_some_and(|token| if domains.mouse { cursor::activate(token, setup) } else { setup });
            let success = token.is_some_and(|token| registration::activate(token, cursor_ready));
            let done = self.event(PolicyEvent::SetupFinished { epoch, success });
            self.setting_up.store(false, Ordering::Release);
            if let Some(done) = done {
                self.effects(done)
            }
            if !success {
                self.report(InputReason::SetupFailed, None);
            } else if reason != InputReason::Requested {
                self.report(reason, None);
            }
        }
        if let Some(Transition::Release { epoch, reason }) = e.transition {
            self.restore_intent
                .store(!matches!(reason, InputReason::FocusLost | InputReason::WindowDestroyed), Ordering::Release);
            self.restore_epoch.store(epoch, Ordering::Release);
            self.report(reason, None);
            self.retry_restore();
        }
    }
    fn retry_restore(&self) {
        let epoch = self.restore_epoch.load(Ordering::Acquire);
        if epoch == 0 {
            return;
        }
        // SAFETY: window queries have no pointer preconditions.
        let foreground = self.restore_intent.load(Ordering::Acquire)
            // SAFETY: foreground query has no arguments.
            && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as usize == self.hwnd();
        // SAFETY: opaque handle is only queried.
        let alive = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(self.hwnd() as HWND) } != 0;
        let restored = bypass(|| {
            let raw = registration::restore();
            let cursor = cursor::restore(foreground, alive);
            raw && cursor
        });
        if restored {
            self.restore_epoch.store(0, Ordering::Release);
            if let Some(done) = self.event(PolicyEvent::Restored { epoch }) {
                self.effects(done);
            }
        }
    }
}
