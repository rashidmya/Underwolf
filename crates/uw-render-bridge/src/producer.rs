use std::{
    collections::{HashMap, VecDeque},
    net::Shutdown,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use uw_frame_channel::{
    Budget, ConsumerConnection, FrameMapping, Patch, PeerIdentity, PublishResult, SessionServer,
    recv_message, send_message,
};
use uw_frame_proto::*;
const LIMIT: usize = 128;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEvent {
    Ready {
        session: SessionId,
    },
    Registered {
        key: AllocationKey,
    },
    Consumed {
        key: AllocationKey,
        seq: u64,
    },
    Retired {
        key: AllocationKey,
    },
    NeedBaseline {
        widget: WidgetKey,
    },
    Swapchain {
        generation: u64,
        width: u32,
        height: u32,
        ready: bool,
    },
    Progress {
        swapchain_generation: u64,
        completed_draw: u64,
        monotonic_ns: u64,
    },
    Disconnected {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BridgeStats {
    pub cpu_bytes: u64,
    pub publications: u64,
    pub coalesced_hints: u64,
    pub live_pixel_bytes: u64,
    pub retired_pixel_bytes: u64,
}
struct Record {
    mapping: Arc<FrameMapping>,
    consumed: u64,
}
struct Packet {
    message: Message,
    mapping: Option<Arc<FrameMapping>>,
}
struct State {
    session: Option<SessionId>,
    alignment: u64,
    peer: PeerIdentity,
    stream: Option<Arc<UnixStream>>,
    budget: Budget,
    next: u64,
    active: HashMap<u32, Record>,
    retired: HashMap<AllocationKey, Arc<FrameMapping>>,
    critical: VecDeque<Packet>,
    dirty: HashMap<AllocationKey, u64>,
    layouts: HashMap<WidgetKey, Message>,
    revisions: HashMap<WidgetKey, u64>,
    dimming: Option<Message>,
    dim_revision: u64,
    events: VecDeque<BridgeEvent>,
    stats: BridgeStats,
    fault: Option<String>,
}
impl State {
    fn event(&mut self, event: BridgeEvent) {
        self.events.retain(|old| {
            !matches!((old,&event),
            (BridgeEvent::Consumed{key:a,..},BridgeEvent::Consumed{key:b,..}) if a==b)
                && !matches!(
                    (old, &event),
                    (BridgeEvent::Progress { .. }, BridgeEvent::Progress { .. })
                )
        });
        if self.events.len() == LIMIT {
            self.fault = Some("bridge event queue capacity".into());
            self.events.clear();
            self.events.push_back(BridgeEvent::Unavailable {
                reason: "bridge event queue capacity".into(),
            });
        } else {
            self.events.push_back(event);
        }
    }
    fn current(&self, key: AllocationKey) -> Option<&Record> {
        self.active
            .get(&key.widget.id)
            .filter(|r| r.mapping.header().key == key)
    }
    fn known(&self, key: AllocationKey) -> bool {
        self.current(key).is_some() || self.retired.contains_key(&key)
    }
    fn packet(&mut self) -> Option<Packet> {
        if let Some(p) = self.critical.pop_front() {
            return Some(p);
        }
        let message = if let Some((&key, &produced)) = self.dirty.iter().next() {
            self.dirty.remove(&key);
            Message::Dirty { key, produced }
        } else if let Some((&key, &m)) = self.layouts.iter().next() {
            self.layouts.remove(&key);
            m
        } else {
            self.dimming.take()?
        };
        Some(Packet {
            message,
            mapping: None,
        })
    }
    fn reconcile(&mut self) -> Result<()> {
        let mut events = Vec::new();
        for record in self.active.values_mut() {
            match record.mapping.try_state() {
                Ok(s) if s.phase == Phase::Invalid => return Err(FrameError::OwnerDied),
                Ok(s) if s.consumed > record.consumed => {
                    record.consumed = s.consumed;
                    events.push(BridgeEvent::Consumed {
                        key: s.key,
                        seq: s.consumed,
                    });
                }
                Ok(_) | Err(FrameError::Busy) => {}
                Err(e) => return Err(e),
            }
        }
        for e in events {
            self.event(e);
        }
        Ok(())
    }
    fn incoming(&mut self, message: Message) -> Result<()> {
        match message {
            Message::Registered { key } if self.known(key) => {
                self.event(BridgeEvent::Registered { key })
            }
            Message::Consumed { key, .. } if self.known(key) => self.reconcile()?,
            Message::Retired { key } if self.retired.remove(&key).is_some() => {
                if !self
                    .active
                    .values()
                    .any(|r| r.mapping.header().key.widget == key.widget)
                    && !self.retired.keys().any(|k| k.widget == key.widget)
                {
                    self.revisions.remove(&key.widget);
                }
                self.event(BridgeEvent::Retired { key });
            }
            Message::NeedBaseline { widget }
                if self
                    .active
                    .get(&widget.id)
                    .is_some_and(|r| r.mapping.header().key.widget == widget) =>
            {
                self.event(BridgeEvent::NeedBaseline { widget })
            }
            Message::Swapchain {
                generation,
                width,
                height,
                ready,
            } => self.event(BridgeEvent::Swapchain {
                generation,
                width,
                height,
                ready,
            }),
            Message::Progress {
                swapchain_generation,
                completed_draw,
                monotonic_ns,
            } => self.event(BridgeEvent::Progress {
                swapchain_generation,
                completed_draw,
                monotonic_ns,
            }),
            Message::Unavailable { key, reason } if key.is_none_or(|key| self.known(key)) => {
                self.event(BridgeEvent::Unavailable {
                    reason: format!("consumer unavailable: {reason}"),
                });
                self.fault = Some(format!("consumer unavailable: {reason}"));
            }
            _ => return Err(FrameError::Invalid),
        }
        Ok(())
    }
    fn clear_session(&mut self, reason: String) {
        self.session = None;
        self.stream = None;
        self.active.clear();
        self.retired.clear();
        self.critical.clear();
        self.dirty.clear();
        self.layouts.clear();
        self.revisions.clear();
        self.dimming = None;
        self.dim_revision = 0;
        self.fault = None;
        // A disconnect ends ownership; abandoned backing is never reused. Any
        // consumer GPU reference owns its separate descriptor/mapping lifetime.
        self.event(BridgeEvent::Disconnected { reason });
    }
}
pub struct ProducerSession {
    state: Arc<Mutex<State>>,
    wake: SyncSender<()>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl ProducerSession {
    pub fn bind(peer: PeerIdentity) -> Result<Self> {
        Self::from_server(SessionServer::bind(peer)?, peer)
    }
    pub fn bind_path(path: &Path, peer: PeerIdentity) -> Result<Self> {
        Self::from_server(SessionServer::bind_path(path, peer)?, peer)
    }
    fn from_server(mut server: SessionServer, peer: PeerIdentity) -> Result<Self> {
        let state = Arc::new(Mutex::new(State {
            session: None,
            alignment: 4096,
            peer,
            stream: None,
            budget: Budget::new(SHARED_BUDGET),
            next: 0,
            active: HashMap::new(),
            retired: HashMap::new(),
            critical: VecDeque::new(),
            dirty: HashMap::new(),
            layouts: HashMap::new(),
            revisions: HashMap::new(),
            dimming: None,
            dim_revision: 0,
            events: VecDeque::new(),
            stats: BridgeStats::default(),
            fault: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, rx) = mpsc::sync_channel(1);
        let shared = state.clone();
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("uw-render-channel".into())
            .spawn(move || {
                let mut connection: Option<ConsumerConnection> = None;
                while !stopping.load(Ordering::Acquire) {
                    if connection.is_none() {
                        match server.try_accept() {
                            Ok(c) => match c.stream.try_clone() {
                                Ok(stream) => {
                                    let mut s = shared.lock().unwrap();
                                    s.session = Some(c.session);
                                    s.alignment = c.alignment;
                                    s.stream = Some(Arc::new(stream));
                                    s.event(BridgeEvent::Ready { session: c.session });
                                    connection = Some(c);
                                }
                                Err(_) => {
                                    shared.lock().unwrap().event(BridgeEvent::Unavailable {
                                        reason: "socket clone failed".into(),
                                    });
                                }
                            },
                            Err(FrameError::Busy) => {}
                            // A rejected peer/handshake must not stop future accepts.
                            Err(_) => {}
                        }
                    }
                    if let Some(c) = connection.as_ref() {
                        let outcome = (|| -> Result<()> {
                            for _ in 0..64 {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                let packet = {
                                    let mut s = shared.lock().unwrap();
                                    if s.fault.is_some() {
                                        return Err(FrameError::Capacity);
                                    }
                                    s.packet()
                                };
                                let Some(packet) = packet else {
                                    break;
                                };
                                let descriptors = packet.mapping.as_ref().map(|mapping| {
                                    // SAFETY: descriptors go only to the authenticated
                                    // native peer implementing the locked ABI.
                                    unsafe { mapping.descriptors() }
                                });
                                send_message(
                                    &c.stream,
                                    &packet.message,
                                    descriptors.as_ref().map_or(&[], |fds| &fds[..]),
                                )?;
                            }
                            // Drain a bounded readable batch, so supported multi-widget
                            // traffic cannot backlog behind one packet per 5ms tick.
                            for _ in 0..64 {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                let mut poll = libc::pollfd {
                                    fd: c.stream.as_raw_fd(),
                                    events: libc::POLLIN,
                                    revents: 0,
                                };
                                // SAFETY: poll receives one valid stack entry for this call.
                                let ready = unsafe { libc::poll(&mut poll, 1, 0) };
                                if ready < 0 {
                                    return Err(FrameError::Io);
                                }
                                if ready == 0 {
                                    break;
                                }
                                if ready > 0 {
                                    let (message, fds) = recv_message(&c.stream)?;
                                    if !fds.is_empty() {
                                        return Err(FrameError::Invalid);
                                    }
                                    shared.lock().unwrap().incoming(message)?;
                                }
                            }
                            shared.lock().unwrap().reconcile()?;
                            Ok(())
                        })();
                        if let Err(e) = outcome {
                            connection = None;
                            shared.lock().unwrap().clear_session(format!("{e:?}"));
                        }
                    }
                    let _ = rx.recv_timeout(Duration::from_millis(5));
                }
                drop(connection);
                shared.lock().unwrap().clear_session("stopped".into());
            })
            .map_err(|_| FrameError::Io)?;
        Ok(Self {
            state,
            wake,
            stop,
            worker: Some(worker),
        })
    }
    pub fn create_widget(
        &self,
        widget: WidgetKey,
        width: u32,
        height: u32,
    ) -> Result<AllocationKey> {
        widget.validate()?;
        let mut s = self.state.lock().unwrap();
        let session = s.session.ok_or(FrameError::Stale)?;
        if s.fault.is_some() {
            return Err(FrameError::Capacity);
        }
        if s.active.len() + s.retired.len() >= LIMIT
            || s.critical.len() >= LIMIT
            || (!s.active.contains_key(&widget.id) && s.active.len() >= MAX_WIDGETS as usize)
        {
            return Err(FrameError::Capacity);
        }
        let generation = s.next.checked_add(1).ok_or(FrameError::Overflow)?;
        let key = AllocationKey {
            session,
            widget,
            generation,
        };
        let layout = FrameLayout::new(width, height, s.alignment)?;
        let mapping = Arc::new(FrameMapping::create_in(&s.budget, key, layout, s.peer)?);
        if let Some(old) = s.active.remove(&widget.id) {
            let old_key = old.mapping.header().key;
            s.dirty.remove(&old_key);
            s.layouts.remove(&old_key.widget);
            s.retired.insert(old_key, old.mapping);
        }
        s.next = generation;
        s.active.insert(
            widget.id,
            Record {
                mapping: mapping.clone(),
                consumed: 0,
            },
        );
        s.critical.push_back(Packet {
            message: Message::Register(mapping.header()),
            mapping: Some(mapping),
        });
        drop(s);
        let _ = self.wake.try_send(());
        Ok(key)
    }
    pub fn publish_patch(
        &self,
        key: AllocationKey,
        seq: u64,
        patch: Patch<'_>,
    ) -> Result<PublishResult> {
        let mapping = {
            let s = self.state.lock().unwrap();
            let Some(record) = s.current(key) else {
                return Ok(PublishResult::Stale);
            };
            record.mapping.clone()
        };
        let result = mapping.try_publish(seq, patch)?;
        if let PublishResult::Published { bytes, .. } = result {
            let mut s = self.state.lock().unwrap();
            if s.current(key).is_none() {
                return Ok(PublishResult::Stale);
            }
            s.stats.cpu_bytes += bytes;
            s.stats.publications += 1;
            if s.dirty.insert(key, seq).is_some() {
                s.stats.coalesced_hints += 1;
            }
            drop(s);
            let _ = self.wake.try_send(());
        }
        Ok(result)
    }
    pub fn can_publish(&self, key: AllocationKey) -> bool {
        let s = self.state.lock().unwrap();
        s.fault.is_none()
            && s.current(key).is_some_and(|r| {
                r.mapping
                    .try_state()
                    .is_ok_and(|s| matches!(s.phase, Phase::Idle | Phase::Ready))
            })
    }
    pub fn control(&self, message: Message) -> Result<()> {
        message.validate()?;
        let mut s = self.state.lock().unwrap();
        if s.session.is_none() {
            return Err(FrameError::Stale);
        }
        match message {
            Message::Layout {
                widget, revision, ..
            }
            | Message::Remove { widget, revision } => {
                if !s
                    .active
                    .get(&widget.id)
                    .is_some_and(|r| r.mapping.header().key.widget == widget)
                    || s.revisions.get(&widget).is_some_and(|old| revision <= *old)
                {
                    return Err(FrameError::Stale);
                }
                if matches!(message, Message::Remove { .. }) {
                    if s.critical.len() == LIMIT {
                        s.fault = Some("control queue capacity".into());
                        return Err(FrameError::Capacity);
                    }
                    let old = s.active.remove(&widget.id).unwrap();
                    let key = old.mapping.header().key;
                    s.retired.insert(key, old.mapping);
                    s.dirty.remove(&key);
                    s.layouts.remove(&widget);
                    s.critical.push_back(Packet {
                        message,
                        mapping: None,
                    });
                } else {
                    s.layouts.insert(widget, message);
                }
                s.revisions.insert(widget, revision);
            }
            Message::Dimming { revision, .. } if revision > s.dim_revision => {
                s.dim_revision = revision;
                s.dimming = Some(message);
            }
            Message::Dimming { .. } => return Err(FrameError::Stale),
            _ => return Err(FrameError::Invalid),
        }
        drop(s);
        let _ = self.wake.try_send(());
        Ok(())
    }
    pub fn drain_events(&self) -> Vec<BridgeEvent> {
        self.state.lock().unwrap().events.drain(..).collect()
    }
    pub fn stats(&self) -> BridgeStats {
        let s = self.state.lock().unwrap();
        BridgeStats {
            live_pixel_bytes: s
                .active
                .values()
                .map(|r| r.mapping.header().layout.pixel_len)
                .sum(),
            retired_pixel_bytes: s
                .retired
                .values()
                .map(|m| m.header().layout.pixel_len)
                .sum(),
            ..s.stats
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let stream = self.state.lock().unwrap().stream.clone();
        if let Some(stream) = stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for ProducerSession {
    fn drop(&mut self) {
        self.stop();
    }
}
