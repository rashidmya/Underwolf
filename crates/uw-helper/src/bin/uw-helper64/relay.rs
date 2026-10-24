//! The relay (design 2a "Components and flow", "Threads"): after the bootstrap's first `hello`,
//! one loop waits on everything at once: queued stdin commands, `bell-host`, the bootstrap pipe
//! and the overlay pipe. It relays commands to the pipes and the state block, and replies,
//! connections and bells back as JSON-lines events. A watcher thread reports `game_exited`.
//!
//! Every command gets one reply carrying its `request_id`. No queueing while disconnected:
//! `load_overlay` needs the bootstrap (`bootstrap_not_connected`), `set_state` and `ping` need the
//! overlay (`overlay_not_connected`). A `load_overlay` without its `status` in time is
//! `status_timeout`, a `ping` without its `pong` in time `overlay_not_responding`; a reply that
//! comes after that is discarded.

use std::io::Write;

use uw_game_channel::objects::{LockError, Objects, LOCK_TIMEOUT_MS};
use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_helper::pending::Pending;
use uw_pipe_proto::bells::{bit_names, to_host};
use uw_pipe_proto::jsonl::{Command, Event};
use uw_pipe_proto::pipe::{
    check_overlay_first, decode, decode_overlay, encode, encode_overlay, Message, OverlayFirst, OverlayMessage,
    MAX_PATH_UNITS, PROTOCOL_VERSION,
};
use uw_pipe_proto::state;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForMultipleObjects, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
};

use crate::commands::Commands;
use crate::pipe_server::{now_ms, PipeEvent, PipeServer};
use crate::wait_ms;
use crate::win32::Channel;

/// The loop wakes at least this often, so a pipe whose connect could not start retries.
const MAX_WAIT_MS: u64 = 1_000;

/// Write one event line to stdout (whole lines, so threads never interleave).
pub(crate) fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_line());
    let _ = out.flush();
}

struct Relay<'a> {
    objects: &'a Objects,
    bootstrap: &'a mut PipeServer,
    overlay: &'a mut PipeServer,
    status_timeout_ms: u64,
    ping_timeout_ms: u64,
    /// The bootstrap sent its `hello` on the current connection.
    bootstrap_up: bool,
    /// A client connected to the bootstrap pipe and its `hello` is next.
    bootstrap_hello_due: bool,
    overlay_up: bool,
    overlay_hello_due: bool,
    loads: Pending,
    /// Pings sent to the overlay, by request id (the ping's nonce), with their deadlines.
    pings: Pending,
}

/// Run the relay until the process exits (stdin closes, or the game exits). On entry the bootstrap
/// is connected and its `hello` has been taken and reported.
pub(crate) fn run(
    pid: u32,
    commands: &Commands,
    channel: &mut Channel,
    bootstrap: &mut PipeServer,
    status_timeout_ms: u64,
    ping_timeout_ms: u64,
) {
    spawn_game_watcher(pid);
    let mut r = Relay {
        objects: &channel.objects,
        bootstrap,
        overlay: &mut channel.overlay,
        status_timeout_ms,
        ping_timeout_ms,
        bootstrap_up: true,
        bootstrap_hello_due: false,
        overlay_up: false,
        overlay_hello_due: false,
        loads: Pending::new(),
        pings: Pending::new(),
    };
    loop {
        r.serve_bootstrap();
        r.serve_overlay();
        while let Ok(cmd) = commands.rx.try_recv() {
            r.command(cmd);
        }
        r.take_bells();
        for id in r.loads.expire(now_ms()) {
            emit(&Event::reply_error(id, "status_timeout"));
        }
        // Owner decision D1 (design "Failures"): a ping the overlay does not answer in time.
        for id in r.pings.expire(now_ms()) {
            emit(&Event::reply_error(id, "overlay_not_responding"));
        }
        let handles = [commands.event, r.objects.bell_host(), r.bootstrap.event(), r.overlay.event()];
        let next = [r.loads.next_deadline(), r.pings.next_deadline()].into_iter().flatten().min();
        let timeout = next.map_or(MAX_WAIT_MS, |d| d.saturating_sub(now_ms()).min(MAX_WAIT_MS));
        // SAFETY: waits on four handles this process owns; the result only says "something may be
        // ready", and every source is polled at the top of the loop.
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, wait_ms(timeout)) };
    }
}

impl Relay<'_> {
    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::InputAck { request_id, .. } => emit(&Event::reply_error(request_id, "input_unavailable")),
            Command::LoadOverlay { request_id, path } => {
                if path.encode_utf16().count() > MAX_PATH_UNITS {
                    emit(&Event::reply_error(request_id, "path_too_long"));
                } else if !self.bootstrap_up {
                    emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                } else if self.bootstrap.write(&encode(&Message::LoadOverlay { request_id, path })) {
                    // The id travels to the bootstrap, which echoes it in `status`.
                    self.loads.add(request_id, now_ms().saturating_add(self.status_timeout_ms));
                } else {
                    // A failed write is `bootstrap_disconnected`, not `game_exited` (design 2a).
                    emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                    self.drop_bootstrap();
                }
            }
            Command::SetState { input_epoch: _, request_id, edit_mode, visible } => {
                if !self.overlay_up {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                    return;
                }
                let change = match self.objects.lock(LOCK_TIMEOUT_MS) {
                    Ok(mut g) => state::apply(g.bytes_mut(), edit_mode, visible),
                    Err(LockError::Busy) => {
                        emit(&Event::reply_error(request_id, "state_busy"));
                        return;
                    }
                    Err(LockError::BadHeader(h)) => {
                        eprintln!("[uw-helper64] state block: {h}");
                        emit(&Event::reply_error(request_id, "version_mismatch"));
                        return;
                    }
                };
                // Nothing changed: the current seq, no bump, no bell (design 2a).
                if change.bits != 0 {
                    self.objects.ring_game(change.bits);
                }
                emit(&Event::StateSet { request_id, seq: change.seq });
            }
            Command::Ping { request_id } => {
                if !self.overlay_up {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                } else if self.overlay.write(&encode_overlay(&OverlayMessage::Ping { nonce: request_id })) {
                    self.pings.add(request_id, now_ms().saturating_add(self.ping_timeout_ms));
                } else {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                    self.drop_overlay();
                }
            }
        }
    }

    /// Swap `to_host` and report the bits with `applied_seq`. If the lock is busy the bits go
    /// back and the bell is rung again, so the next pass retries (a bell is not a command, so it
    /// is never dropped as `state_busy`).
    fn take_bells(&mut self) {
        let bits = self.objects.take_to_host();
        if bits == 0 {
            return;
        }
        match self.objects.lock(LOCK_TIMEOUT_MS) {
            Ok(g) => {
                let applied_seq = state::read(g.bytes()).applied_seq;
                drop(g);
                emit(&Event::Bell { bits: bit_names(bits, &to_host::NAMES), applied_seq });
            }
            Err(e) => {
                eprintln!("[uw-helper64] bell: state lock {e:?}; retrying");
                self.objects.ring_host(bits);
            }
        }
    }

    fn serve_bootstrap(&mut self) {
        while let Some(ev) = self.bootstrap.poll() {
            match ev {
                PipeEvent::Connected => self.bootstrap_hello_due = true,
                // A bootstrap that reconnected (design: it retries every second).
                PipeEvent::Message(bytes) if self.bootstrap_hello_due => {
                    self.bootstrap_hello_due = false;
                    match hello_wait_from_bytes(&bytes) {
                        HelloWait::Hello { protocol_version, overlay_loaded, overlay_path }
                            if protocol_version == PROTOCOL_VERSION =>
                        {
                            self.bootstrap_up = true;
                            emit(&Event::BootstrapConnected { overlay_loaded, overlay_path });
                        }
                        HelloWait::Hello { .. } => {
                            emit(&Event::error("version_mismatch"));
                            self.bootstrap.disconnect();
                        }
                        _ => {
                            emit(&Event::error("pipe_error"));
                            self.bootstrap.disconnect();
                        }
                    }
                }
                PipeEvent::Message(bytes) => match decode(&bytes) {
                    Ok(Message::Status { request_id, code, detail, message }) => {
                        if self.loads.take(request_id) {
                            emit(&Event::Status { request_id, code: code.as_str().into(), detail, message });
                        } else {
                            // A late reply to a request that already timed out (design 2a).
                            eprintln!("[uw-helper64] discarding a status for request {request_id}: not pending");
                        }
                    }
                    _ => {
                        emit(&Event::error("pipe_error"));
                        self.drop_bootstrap();
                    }
                },
                PipeEvent::Oversized => {
                    emit(&Event::error("pipe_error"));
                    self.bootstrap_lost();
                }
                PipeEvent::Disconnected => self.bootstrap_lost(),
            }
        }
    }

    fn serve_overlay(&mut self) {
        while let Some(ev) = self.overlay.poll() {
            match ev {
                PipeEvent::Connected => self.overlay_hello_due = true,
                PipeEvent::Message(bytes) if self.overlay_hello_due => {
                    self.overlay_hello_due = false;
                    match check_overlay_first(&bytes) {
                        OverlayFirst::Hello { module_version } => {
                            self.overlay_up = true;
                            emit(&Event::OverlayConnected { module_version });
                        }
                        OverlayFirst::InputHello { .. } | OverlayFirst::VersionMismatch => {
                            emit(&Event::error("version_mismatch"));
                            self.overlay.disconnect();
                        }
                        OverlayFirst::Bad(why) => {
                            eprintln!("[uw-helper64] overlay pipe: {why}");
                            emit(&Event::error("pipe_error"));
                            self.overlay.disconnect();
                        }
                    }
                }
                PipeEvent::Message(bytes) => match decode_overlay(&bytes) {
                    Ok(OverlayMessage::Pong { nonce, seen_seq }) => {
                        if self.pings.take(nonce) {
                            emit(&Event::Pong { request_id: nonce, seen_seq });
                        } else {
                            // A late pong to a ping that already got `overlay_not_responding`.
                            eprintln!("[uw-helper64] discarding a pong for {nonce}: not pending");
                        }
                    }
                    // An unknown type or a bad message: `pipe_error`, drop it, listen again.
                    _ => {
                        emit(&Event::error("pipe_error"));
                        self.drop_overlay();
                    }
                },
                // An oversized message is a bad message too (design "Failures"): `pipe_error`;
                // the pipe server already dropped it and listens again.
                PipeEvent::Oversized => {
                    emit(&Event::error("pipe_error"));
                    self.overlay_lost();
                }
                PipeEvent::Disconnected => self.overlay_lost(),
            }
        }
    }

    fn drop_bootstrap(&mut self) {
        self.bootstrap.disconnect();
        self.bootstrap_lost();
    }

    fn bootstrap_lost(&mut self) {
        if self.bootstrap_up {
            emit(&Event::BootstrapDisconnected);
        }
        self.bootstrap_up = false;
        self.bootstrap_hello_due = false;
        for id in self.loads.drain() {
            emit(&Event::reply_error(id, "bootstrap_not_connected"));
        }
    }

    fn drop_overlay(&mut self) {
        self.overlay.disconnect();
        self.overlay_lost();
    }

    fn overlay_lost(&mut self) {
        if self.overlay_up {
            emit(&Event::OverlayDisconnected);
        }
        self.overlay_up = false;
        self.overlay_hello_due = false;
        for id in self.pings.drain() {
            emit(&Event::reply_error(id, "overlay_not_connected"));
        }
    }
}

/// Watch the game process; when it ends, report `game_exited` and exit 0 (design failure table).
fn spawn_game_watcher(pid: u32) {
    std::thread::spawn(move || {
        // SAFETY: opens the game for SYNCHRONIZE and waits for it to exit.
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return;
            }
            WaitForSingleObject(h, INFINITE);
            CloseHandle(h);
        }
        emit(&Event::GameExited);
        std::process::exit(0);
    });
}
