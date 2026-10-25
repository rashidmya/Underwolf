//! Negotiated input session. The legacy state-only worker remains separate.
use uw_pipe_proto::{
    input::*,
    pipe::OverlayMessage,
    routing::{ROUTING_VERSION, RoutingConfig, RoutingMessage, RoutingRequest},
    state::StateV1,
};
#[derive(Debug)]
pub enum SessionEvent {
    Connected,
    Configure(RoutingConfig),
    DeferHidden,
    RouteRequest(RoutingRequest),
    Disconnected,
    State { bits: u64, state: StateV1 },
    Capture(CaptureRequest),
    Visibility { state_seq: u64, on: bool },
    ApplyCapture { request: CaptureRequest, visible: bool, state_seq: u64 },
    Ack(InputAck),
    Ping(u32),
}
#[derive(Debug)]
pub struct SessionError;
#[derive(Default)]
pub struct Session {
    routing: bool,
    connected: bool,
    seen: u64,
}
/// Routing has its own negotiation; legacy fixtures continue to use Session explicitly.
#[derive(Default)]
pub struct RoutingSession(Session);
impl RoutingSession {
    pub fn receive(&mut self, message: OverlayMessage) -> Result<SessionEvent, SessionError> {
        self.0.routing = true;
        self.0.receive(message)
    }
    pub fn connected(&self) -> bool {
        self.0.connected()
    }
    pub fn seen_seq(&self) -> u64 {
        self.0.seen_seq()
    }
    pub fn applied(&mut self, seq: u64) {
        self.0.applied(seq);
    }
    pub fn disconnect(&mut self) -> SessionEvent {
        self.0.disconnect()
    }
}
impl Session {
    fn receive_routing(&mut self, message: OverlayMessage) -> Result<SessionEvent, SessionError> {
        if !self.connected {
            return match message {
                OverlayMessage::Routing(RoutingMessage::Welcome { input_version: ROUTING_VERSION }) => {
                    self.connected = true;
                    Ok(SessionEvent::Connected)
                }
                _ => Err(SessionError),
            };
        }
        if let OverlayMessage::Routing(message) = &message {
            message.validate().map_err(|_| SessionError)?;
        }
        match message {
            OverlayMessage::Routing(RoutingMessage::Configure(config)) => Ok(SessionEvent::Configure(config)),
            OverlayMessage::Routing(RoutingMessage::Request(request)) => Ok(SessionEvent::RouteRequest(request)),
            OverlayMessage::Routing(RoutingMessage::Visibility { state_seq, on }) => {
                Ok(SessionEvent::Visibility { state_seq, on })
            }
            OverlayMessage::Routing(RoutingMessage::Ack(ack)) => Ok(SessionEvent::Ack(ack)),
            OverlayMessage::Ping { nonce } => Ok(SessionEvent::Ping(nonce)),
            _ => Err(SessionError),
        }
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn seen_seq(&self) -> u64 {
        self.seen
    }
    pub fn applied(&mut self, seq: u64) {
        self.seen = self.seen.max(seq)
    }
    pub fn disconnect(&mut self) -> SessionEvent {
        self.connected = false;
        SessionEvent::Disconnected
    }
    pub fn receive(&mut self, message: OverlayMessage) -> Result<SessionEvent, SessionError> {
        if self.routing {
            return self.receive_routing(message);
        }
        if !self.connected {
            return match message {
                OverlayMessage::Input(InputMessage::Welcome { input_version: INPUT_VERSION }) => {
                    self.connected = true;
                    Ok(SessionEvent::Connected)
                }
                _ => Err(SessionError),
            };
        }
        match message {
            OverlayMessage::Input(InputMessage::Visibility { state_seq, on }) => {
                Ok(SessionEvent::Visibility { state_seq, on })
            }
            OverlayMessage::Input(InputMessage::ApplyCapture { request, visible }) => {
                Ok(SessionEvent::ApplyCapture { request, visible, state_seq: request.state_seq })
            }
            OverlayMessage::Input(InputMessage::Capture(r)) => Ok(SessionEvent::Capture(r)),
            OverlayMessage::Input(InputMessage::Ack(a)) => Ok(SessionEvent::Ack(a)),
            OverlayMessage::Ping { nonce } => Ok(SessionEvent::Ping(nonce)),
            _ => Err(SessionError),
        }
    }
}
/// A hidden snapshot may coalesce an older release, but can never authorize an older entry.
pub struct RoutingSnapshot {
    pub request: RoutingRequest,
    pub defer_visibility: bool,
}
impl RoutingSnapshot {
    pub fn new(mut request: RoutingRequest, actual_sequence: u64, visible: bool) -> Self {
        let defer_visibility = !visible
            && request.action == uw_pipe_proto::routing::RoutingAction::LeaveEdit
            && request.state_seq.is_some_and(|sequence| sequence <= actual_sequence);
        if defer_visibility {
            request.state_seq = Some(actual_sequence);
        }
        Self { request, defer_visibility }
    }
}
#[cfg(windows)]
mod win {
    use super::*;
    use crate::{
        objects::Objects,
        worker::{Client, Read},
    };
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{Receiver, SyncSender, sync_channel},
        },
    };
    use uw_pipe_proto::{
        bells::to_host,
        names::overlay_pipe_name,
        pipe::{decode_overlay, encode_input},
        state,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, Sleep};
    pub type InputCallbacks = Box<dyn FnMut(SessionEvent) + Send + 'static>;
    #[derive(Debug)]
    pub struct Full;
    struct PendingVisibility {
        request: RoutingRequest,
        deadline: u64,
        hidden: Option<u64>,
    }
    enum Outgoing {
        Message(OverlayMessage),
        Applied(u64),
    }
    #[derive(Clone)]
    pub struct InputEndpoint {
        routing: bool,
        tx: SyncSender<Outgoing>,
        connected: Arc<AtomicBool>,
        broken: Arc<AtomicBool>,
    }
    impl InputEndpoint {
        pub fn try_send(&self, message: OverlayMessage) -> Result<(), Full> {
            if !self.connected.load(Ordering::Acquire) {
                return Err(Full);
            }
            let allowed = if self.routing {
                matches!(
                    &message,
                    OverlayMessage::Routing(
                        RoutingMessage::State(_)
                            | RoutingMessage::Configured(_)
                            | RoutingMessage::Events(_)
                            | RoutingMessage::Challenge(_)
                            | RoutingMessage::Result(_)
                            | RoutingMessage::Coverage(_)
                    )
                )
            } else {
                matches!(
                    &message,
                    OverlayMessage::Input(
                        InputMessage::State(_)
                            | InputMessage::Batch(_)
                            | InputMessage::Challenge(_)
                            | InputMessage::Result(_)
                    )
                )
            };
            if !allowed {
                return Err(Full);
            }
            self.tx.try_send(Outgoing::Message(message)).map_err(|_| {
                self.broken.store(true, Ordering::Release);
                Full
            })
        }
        pub fn disconnect(&self) {
            self.connected.store(false, Ordering::Release);
            self.broken.store(true, Ordering::Release);
        }
        pub fn applied(&self, seq: u64) -> Result<(), Full> {
            if !self.connected() {
                return Err(Full);
            }
            self.tx.try_send(Outgoing::Applied(seq)).map_err(|_| {
                self.broken.store(true, Ordering::Release);
                Full
            })
        }
        pub fn connected(&self) -> bool {
            self.connected.load(Ordering::Acquire)
        }
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    pub fn start_input(module_version: u32, callback: InputCallbacks) -> std::io::Result<InputEndpoint> {
        start_worker(module_version, false, callback)
    }
    pub fn start_routing(module_version: u32, callback: InputCallbacks) -> std::io::Result<InputEndpoint> {
        start_worker(module_version, true, callback)
    }
    fn start_worker(
        module_version: u32,
        routing: bool,
        mut callback: InputCallbacks,
    ) -> std::io::Result<InputEndpoint> {
        if STARTED.swap(true, Ordering::AcqRel) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "input worker already runs"));
        }
        let (tx, rx) = sync_channel(64);
        let ep = InputEndpoint {
            routing,
            tx,
            connected: Arc::new(AtomicBool::new(false)),
            broken: Arc::new(AtomicBool::new(false)),
        };
        let state = ep.clone();
        std::thread::Builder::new().name("uw-input-link".into()).spawn(move || {
            let mut session = Session { routing, ..Session::default() };
            let mut objects = None;
            loop {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    serve(module_version, &mut callback, &rx, &state, &mut session, &mut objects)
                }));
                state.connected.store(false, Ordering::Release);
                state.broken.store(false, Ordering::Release);
                let event = session.disconnect();
                let _ = catch_unwind(AssertUnwindSafe(|| callback(event)));
                while rx.try_recv().is_ok() {}
                // SAFETY: worker-only backoff, never on a game hook thread.
                unsafe { Sleep(1000) };
            }
        })?;
        Ok(ep)
    }
    fn serve(
        version: u32,
        callback: &mut InputCallbacks,
        rx: &Receiver<Outgoing>,
        ep: &InputEndpoint,
        session: &mut Session,
        objects: &mut Option<Objects>,
    ) {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let Some(mut client) = Client::connect(&overlay_pipe_name(pid)) else { return };
        let hello = if session.routing {
            OverlayMessage::Routing(RoutingMessage::Hello {
                module_version: version,
                state_layout: 1,
                input_version: ROUTING_VERSION,
            })
        } else {
            OverlayMessage::Input(InputMessage::Hello {
                module_version: version,
                state_layout: 1,
                input_version: INPUT_VERSION,
            })
        };
        if !client.write(&encode_input(&hello).unwrap()) || !client.start_read() {
            return;
        }
        let Some(deadline) = now().checked_add(5000) else { return };
        let mut pending_applied = None;
        let mut pending_visibility: Vec<PendingVisibility> = Vec::with_capacity(32);
        loop {
            if ep.broken.load(Ordering::Acquire) {
                eprintln!("uw-input-link: endpoint revoked");
                return;
            }
            if !session.connected() && now() >= deadline {
                return;
            }
            match client.poll_read() {
                Read::Broken => return,
                Read::Pending => {}
                Read::Message(bytes) => {
                    let Ok(message) = decode_overlay(&bytes) else { return };
                    let event = match session.receive(message) {
                        Ok(event) => event,
                        Err(error) => {
                            eprintln!("uw-input-link: invalid session message: {error:?}");
                            return;
                        }
                    };
                    match event {
                        SessionEvent::Connected => {
                            let new = match Objects::open(pid) {
                                Ok(objects) => objects,
                                Err(error) => {
                                    eprintln!("uw-input-link: opening state objects failed: {error:?}");
                                    return;
                                }
                            };
                            *objects = Some(new);
                            if !session.routing {
                                ep.connected.store(true, Ordering::Release);
                            }
                            callback(SessionEvent::Connected);
                            if session.routing && !apply_actual_visibility(objects, callback) {
                                return;
                            }
                            ep.connected.store(true, Ordering::Release);
                        }
                        SessionEvent::Ping(nonce) => {
                            if !client.write(
                                &encode_input(&OverlayMessage::Pong { nonce, seen_seq: session.seen_seq() }).unwrap(),
                            ) {
                                return;
                            }
                        }
                        SessionEvent::RouteRequest(request) if request.state_seq.is_some() => {
                            let Some(objects) = objects.as_ref() else { return };
                            let Ok(guard) = objects.lock(100) else { return };
                            let state = state::read(guard.bytes());
                            drop(guard);
                            let snapshot = RoutingSnapshot::new(request, state.seq, state.visible);
                            callback(SessionEvent::State { bits: 0, state });
                            if snapshot.request.action == uw_pipe_proto::routing::RoutingAction::LeaveEdit
                                && snapshot.request.state_seq == Some(state.seq)
                            {
                                if pending_visibility.len() == 32 {
                                    return;
                                }
                                let Some(deadline) = now().checked_add(5000) else { return };
                                pending_visibility.push(PendingVisibility {
                                    request: snapshot.request,
                                    deadline,
                                    hidden: snapshot.defer_visibility.then_some(state.seq),
                                });
                            }
                            if snapshot.defer_visibility
                                || pending_visibility.iter().any(|pending| pending.hidden.is_some())
                            {
                                callback(SessionEvent::DeferHidden);
                            } else {
                                callback(SessionEvent::Visibility { state_seq: state.seq, on: state.visible });
                            }
                            callback(SessionEvent::RouteRequest(snapshot.request));
                        }
                        SessionEvent::Visibility { state_seq, on } if session.routing => {
                            let Some(objects) = objects.as_ref() else { return };
                            let Ok(guard) = objects.lock(100) else { return };
                            let state = state::read(guard.bytes());
                            drop(guard);
                            if state_seq > state.seq {
                                return;
                            }
                            callback(SessionEvent::State { bits: 0, state });
                            let mut deferred = false;
                            if !on {
                                for pending in &mut pending_visibility {
                                    if pending.request.state_seq.is_some_and(|sequence| sequence <= state_seq) {
                                        pending.hidden = Some(pending.hidden.unwrap_or(0).max(state_seq));
                                        deferred = true;
                                    }
                                }
                            } else {
                                deferred = pending_visibility.iter().any(|pending| pending.hidden.is_some());
                            }
                            if deferred {
                                callback(SessionEvent::DeferHidden);
                            } else {
                                callback(SessionEvent::Visibility {
                                    state_seq: if on { state.seq } else { state_seq },
                                    on: on && state.visible,
                                });
                            }
                        }
                        SessionEvent::Capture(r) => {
                            let Some(o) = objects.as_ref() else { return };
                            let Ok(g) = o.lock(100) else { return };
                            let state = state::read(g.bytes());
                            drop(g);
                            callback(SessionEvent::ApplyCapture {
                                request: r,
                                visible: state.visible,
                                state_seq: state.seq,
                            });
                        }
                        other => callback(other),
                    }
                    if !client.start_read() {
                        return;
                    }
                }
            }
            if session.connected() {
                if let Some(o) = objects.as_ref() {
                    let bits = o.take_to_game();
                    if bits != 0 {
                        match o.lock(100) {
                            Ok(g) => {
                                let state = state::read(g.bytes());
                                drop(g);
                                callback(SessionEvent::State { bits, state })
                            }
                            Err(_) => o.ring_game(bits),
                        }
                    }
                    if let Some(seq) = pending_applied
                        && let Ok(mut g) = o.lock(0)
                    {
                        state::set_applied_seq(g.bytes_mut(), seq);
                        drop(g);
                        session.applied(seq);
                        o.ring_host(to_host::STATE_APPLIED);
                        pending_applied = None;
                    }
                }
                for _ in 0..16 {
                    match rx.try_recv() {
                        Ok(Outgoing::Message(m)) => {
                            let deferred = if let OverlayMessage::Routing(RoutingMessage::Result(result)) = &m {
                                pending_visibility.iter().position(|pending| {
                                    pending.request.epoch == result.epoch
                                        && pending.request.revision == result.revision
                                        && pending.request.request_id == result.request_id
                                })
                            } else {
                                None
                            };
                            let frame = match encode_input(&m) {
                                Ok(frame) => frame,
                                Err(error) => {
                                    eprintln!("uw-input-link: invalid outgoing message {m:?}: {error:?}");
                                    return;
                                }
                            };
                            if !client.write(&frame) {
                                return;
                            }
                            if let Some(index) = deferred {
                                let pending = pending_visibility.remove(index);
                                if let Some(hidden) = pending.hidden {
                                    pending_visibility.retain(|other| other.request.epoch > pending.request.epoch);
                                    callback(SessionEvent::Visibility { state_seq: hidden, on: false });
                                }
                                if !apply_actual_visibility(objects, callback) {
                                    return;
                                }
                            }
                        }
                        Ok(Outgoing::Applied(seq)) => {
                            pending_applied = Some(pending_applied.unwrap_or(session.seen_seq()).max(seq))
                        }
                        Err(_) => break,
                    }
                }
            }
            if let Some(index) = pending_visibility.iter().position(|pending| pending.deadline <= now()) {
                let pending = pending_visibility.remove(index);
                if let Some(hidden) = pending.hidden {
                    pending_visibility.retain(|other| other.request.epoch > pending.request.epoch);
                    callback(SessionEvent::Visibility { state_seq: hidden, on: false });
                }
                if !apply_actual_visibility(objects, callback) {
                    return;
                }
            }
            // SAFETY: worker sleeps at most 10 ms; lease timer runs independently in the game.
            unsafe { Sleep(10) };
        }
    }
    fn apply_actual_visibility(objects: &Option<Objects>, callback: &mut InputCallbacks) -> bool {
        let Some(objects) = objects.as_ref() else { return false };
        let guard = match objects.lock(100) {
            Ok(guard) => guard,
            Err(error) => {
                eprintln!("uw-input-link: reading visibility failed: {error:?}");
                return false;
            }
        };
        let state = state::read(guard.bytes());
        drop(guard);
        callback(SessionEvent::State { bits: 0, state });
        callback(SessionEvent::Visibility { state_seq: state.seq, on: state.visible });
        true
    }
    fn now() -> u64 {
        // SAFETY: no arguments.
        unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
    }
}
#[cfg(windows)]
pub use win::{Full, InputCallbacks, InputEndpoint, start_input, start_routing};
