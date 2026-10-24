//! Negotiated input session. The legacy state-only worker remains separate.
use uw_pipe_proto::{input::*, pipe::OverlayMessage, state::StateV1};
#[derive(Debug)]
pub enum SessionEvent {
    Connected,
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
    connected: bool,
    seen: u64,
}
impl Session {
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
    enum Outgoing {
        Message(OverlayMessage),
        Applied(u64),
    }
    #[derive(Clone)]
    pub struct InputEndpoint {
        tx: SyncSender<Outgoing>,
        connected: Arc<AtomicBool>,
        broken: Arc<AtomicBool>,
    }
    impl InputEndpoint {
        pub fn try_send(&self, message: OverlayMessage) -> Result<(), Full> {
            if !self.connected.load(Ordering::Acquire) {
                return Err(Full);
            }
            if !matches!(
                &message,
                OverlayMessage::Input(
                    InputMessage::State(_)
                        | InputMessage::Batch(_)
                        | InputMessage::Challenge(_)
                        | InputMessage::Result(_)
                )
            ) {
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
    pub fn start_input(module_version: u32, mut callback: InputCallbacks) -> std::io::Result<InputEndpoint> {
        if STARTED.swap(true, Ordering::AcqRel) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "input worker already runs"));
        }
        let (tx, rx) = sync_channel(64);
        let ep =
            InputEndpoint { tx, connected: Arc::new(AtomicBool::new(false)), broken: Arc::new(AtomicBool::new(false)) };
        let state = ep.clone();
        std::thread::Builder::new().name("uw-input-link".into()).spawn(move || {
            let mut session = Session::default();
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
        let hello = OverlayMessage::Input(InputMessage::Hello {
            module_version: version,
            state_layout: 1,
            input_version: INPUT_VERSION,
        });
        if !client.write(&encode_input(&hello).unwrap()) || !client.start_read() {
            return;
        }
        let deadline = now() + 5000;
        let mut pending_applied = None;
        loop {
            if ep.broken.load(Ordering::Acquire) {
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
                    let Ok(event) = session.receive(message) else { return };
                    match event {
                        SessionEvent::Connected => {
                            let Ok(new) = Objects::open(pid) else { return };
                            *objects = Some(new);
                            ep.connected.store(true, Ordering::Release);
                            callback(SessionEvent::Connected);
                        }
                        SessionEvent::Ping(nonce) => {
                            if !client.write(
                                &encode_input(&OverlayMessage::Pong { nonce, seen_seq: session.seen_seq() }).unwrap(),
                            ) {
                                return;
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
                            let Ok(frame) = encode_input(&m) else { return };
                            if !client.write(&frame) {
                                return;
                            }
                        }
                        Ok(Outgoing::Applied(seq)) => {
                            pending_applied = Some(pending_applied.unwrap_or(session.seen_seq()).max(seq))
                        }
                        Err(_) => break,
                    }
                }
            }
            // SAFETY: worker sleeps at most 10 ms; lease timer runs independently in the game.
            unsafe { Sleep(10) };
        }
    }
    fn now() -> u64 {
        // SAFETY: no arguments.
        unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
    }
}
#[cfg(windows)]
pub use win::{Full, InputCallbacks, InputEndpoint, start_input};
