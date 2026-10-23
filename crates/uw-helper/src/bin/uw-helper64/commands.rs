//! The stdin reader. One thread reads JSON-lines commands for the whole run, from before the hook
//! on, and hands each to the relay through a channel. When stdin closes the helper exits at once,
//! in every phase (design 2a "Failures": no orphan helper keeps the helper mutex).

use std::io::BufRead;
use std::sync::mpsc::{channel, Receiver};

use uw_pipe_proto::jsonl::{Command, Event};

use crate::relay::emit;

pub(crate) struct Commands {
    pub(crate) rx: Receiver<Command>,
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
                Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
            };
            emit(&Event::reply_error(cmd.request_id(), code));
        }
    }
}

pub(crate) fn spawn_stdin_reader() -> Commands {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match Command::parse(&line) {
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
            }
        }
        eprintln!("[uw-helper64] stdin closed; exiting");
        std::process::exit(0);
    });
    Commands { rx }
}
