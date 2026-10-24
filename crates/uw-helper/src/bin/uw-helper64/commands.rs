//! The stdin reader. One thread reads JSON-lines commands for the whole run, from before the hook
//! on, and hands each to the relay through a channel, signalling `event` (an unnamed auto-reset
//! event) so the relay's wait wakes. When stdin closes the helper exits at once, in every phase
//! (design 2a "Failures": no orphan helper keeps the helper mutex).

use std::io::BufRead;
use std::sync::mpsc::{sync_channel, Receiver};

use uw_pipe_proto::jsonl::{Command, Event};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use crate::relay::emit;

pub(crate) struct Commands {
    pub(crate) rx: Receiver<Command>,
    /// Signalled after each command is queued; never closed (it lives as long as the process).
    pub(crate) event: HANDLE,
}

impl Commands {
    /// Answer every command that came before the bootstrap connected, at once (design: no
    /// queueing while disconnected): `load_overlay` needs the bootstrap, `set_state` and `ping`
    /// the overlay. `main` calls this before `bootstrap_connected` goes out, so the host's
    /// reconcile commands, sent in answer to it, reach the relay.
    pub(crate) fn refuse_queued(&self) {
        while let Ok(cmd) = self.rx.try_recv() {
            let code = match cmd {
                Command::LoadOverlay { .. } => "bootstrap_not_connected",
                Command::InputAck { .. } | Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
            };
            emit(&Event::reply_error(cmd.request_id(), code));
        }
    }
}

pub(crate) fn spawn_stdin_reader() -> Result<Commands, String> {
    // SAFETY: an unnamed auto-reset event, initially unsignaled.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
    if event.is_null() {
        return Err("CreateEventW failed".into());
    }
    let (tx, rx) = sync_channel(64);
    let signal = event as usize; // a HANDLE is not `Send`; the event is never closed
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match Command::parse(&line) {
                Ok(cmd) => {
                    if tx.try_send(cmd).is_err() {
                        // A stalled relay must not accumulate input acknowledgements or
                        // block this reader indefinitely. Process loss fails pending
                        // commands at the host and revokes capture in the game.
                        eprintln!("[uw-helper64] command queue overflow; closing session");
                        std::process::exit(1);
                    }
                    // SAFETY: signals the event created above.
                    unsafe { SetEvent(signal as HANDLE) };
                }
                Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
            }
        }
        eprintln!("[uw-helper64] stdin closed; exiting");
        std::process::exit(0);
    });
    Ok(Commands { rx, event })
}
