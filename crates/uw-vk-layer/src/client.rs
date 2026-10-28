//! Authenticated channel and fence polling, strictly outside the present hook.
use crate::{compositor::State, resources::Context};
use std::{
    net::Shutdown,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use uw_frame_channel::{PeerIdentity, recv_message, send_message, session_endpoint};
use uw_frame_proto::*;
pub struct Runtime {
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stream: Arc<Mutex<Option<Arc<UnixStream>>>>,
}
impl Runtime {
    #[cfg(test)]
    pub(crate) fn testing(ctx: Arc<Context>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::new(ctx))),
            stop: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(None),
            stream: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(ctx: Arc<Context>) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(State::new(ctx.clone())));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let stopping = stop.clone();
        let stream = Arc::new(Mutex::new(None::<Arc<UnixStream>>));
        let shared_stream = stream.clone();
        let worker = thread::Builder::new()
            .name("uw-render-client".into())
            .spawn(move || {
                let Ok(identity) = PeerIdentity::current() else {
                    return;
                };
                let Ok(path) = session_endpoint(&identity) else {
                    return;
                };
                let mut connection: Option<(Arc<UnixStream>, PeerIdentity)> = None;
                let mut retry = std::time::Instant::now();
                while !stopping.load(Ordering::Acquire) {
                    if connection.is_none()
                        && std::time::Instant::now() >= retry
                        && shared.lock().unwrap().available
                    {
                        retry = std::time::Instant::now() + Duration::from_millis(100);
                        if let Ok(socket) = UnixStream::connect(&path) {
                            let socket = Arc::new(socket);
                            *shared_stream.lock().unwrap() = Some(socket.clone());
                            let handshake = (|| -> Result<PeerIdentity> {
                                let peer = PeerIdentity::from_stream(&socket)?;
                                if peer.uid != identity.uid {
                                    return Err(FrameError::Invalid);
                                }
                                send_message(
                                    &socket,
                                    &Message::Hello {
                                        pid: identity.pid,
                                        start_ticks: identity.start_ticks,
                                        alignment: ctx.alignment,
                                        mutex_size: 40,
                                        mutex_align: 8,
                                        abi: 1,
                                        capability: 1,
                                    },
                                    &[],
                                )?;
                                let (welcome, fds) = recv_message(&socket)?;
                                let Message::Welcome {
                                    session,
                                    producer_pid,
                                    ..
                                } = welcome
                                else {
                                    return Err(FrameError::Invalid);
                                };
                                if !fds.is_empty() || producer_pid != peer.pid {
                                    return Err(FrameError::Invalid);
                                }
                                shared.lock().unwrap().connected(session);
                                Ok(peer)
                            })();
                            if let Ok(peer) = handshake {
                                connection = Some((socket, peer));
                            } else {
                                *shared_stream.lock().unwrap() = None;
                            }
                        }
                    }
                    let mut failed = false;
                    {
                        let mut state = shared.lock().unwrap();
                        if let Err(_e) = state.poll() {
                            state.fail(3);
                        }
                    }
                    if let Some((socket, peer)) = &connection {
                        let outcome = (|| -> Result<()> {
                            for _ in 0..32 {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                let message = shared.lock().unwrap().outgoing.pop_front();
                                let Some(message) = message else {
                                    break;
                                };
                                send_message(socket, &message, &[])?;
                            }
                            let mut p = libc::pollfd {
                                fd: socket.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            // SAFETY: one valid retained socket and stack poll entry.
                            let ready = unsafe { libc::poll(&mut p, 1, 0) };
                            if ready < 0 {
                                return Err(FrameError::Io);
                            }
                            if ready > 0 {
                                let (message, fds) = recv_message(socket)?;
                                shared.lock().unwrap().incoming(message, fds, *peer)?;
                            }
                            if shared.lock().unwrap().close_connection {
                                return Err(FrameError::Unsupported);
                            }
                            Ok(())
                        })();
                        failed = outcome.is_err();
                    }
                    if failed {
                        if let Some((socket, _)) = connection.take() {
                            let _ = socket.shutdown(Shutdown::Both);
                        }
                        *shared_stream.lock().unwrap() = None;
                        shared.lock().unwrap().disconnected();
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                if let Some((socket, _)) = connection {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                *shared_stream.lock().unwrap() = None;
                shared.lock().unwrap().disconnected();
            })?;
        Ok(Self {
            state,
            stop,
            worker: Mutex::new(Some(worker)),
            stream,
        })
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let stream = self.stream.lock().unwrap().clone();
        if let Some(stream) = stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
