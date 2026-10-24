//! The running host. One thread owns the `Supervisor`: it starts helpers through a `Launch`,
//! writes commands to the current helper's stdin, and gets the helper's stdout lines from a reader
//! thread. Events come out of `Host::recv` in order; it returns `None` once the host has stopped
//! and its helper has exited. Dropping the `Host` stops it (the helper's stdin closes, so it
//! exits: design "Failures", "uw-host exits or dies").

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use uw_pipe_proto::jsonl::{Command as HelperCommand, Event};

use crate::discovery::{helper_command, GameSession};
use crate::driver::Desired;
use crate::supervisor::{Action, Input, Supervisor};
use crate::RequestIds;

/// How the host starts one helper process.
pub trait Launch: Send + 'static {
    /// The helper's command line with `args` after the helper's own (`--data-resume` on a
    /// restart), or the error code (`game_not_found`, `no_wine_session`) when the game or its Wine
    /// session is gone. The host pipes its stdin and stdout.
    fn command(&mut self, args: &[String]) -> Result<Command, String>;
}

/// `uw-helper64.exe` in the game's Wine session. Every start finds the game again by its pid, so
/// a restart after the game has gone stops with `game_not_found`.
pub struct WineLaunch {
    pub session: GameSession,
    pub exe: String,
    pub helper: PathBuf,
    pub bootstrap: PathBuf,
    pub fallback: bool,
}

impl Launch for WineLaunch {
    fn command(&mut self, args: &[String]) -> Result<Command, String> {
        let session =
            GameSession::find_with_proton(&self.exe, Some(self.session.pid), Some(self.session.proton.clone()))
                .map_err(|e| {
                    eprintln!("[uw-host] {e}");
                    e.code().to_string()
                })?;
        let mut cmd = helper_command(&session, &self.helper, &self.exe, &self.bootstrap, self.fallback);
        cmd.args(args);
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
        }
        Ok(cmd)
    }
}

pub struct Host {
    inputs: Sender<Input>,
    events: Receiver<Event>,
    ids: RequestIds,
}

/// Sends commands to a running `Host` from another thread (the `Host` itself receives).
#[derive(Clone)]
pub struct HostHandle {
    inputs: Sender<Input>,
    ids: RequestIds,
}

impl HostHandle {
    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// As `Host::send`.
    pub fn send(&self, cmd: HelperCommand) {
        let _ = self.inputs.send(Input::User(cmd));
    }
}

impl Host {
    /// Start supervising; the first helper starts at once.
    pub fn start(launch: impl Launch, desired: Desired) -> Host {
        let ids = RequestIds::default();
        let (inputs, rx) = channel();
        let (etx, events) = channel();
        let supervisor = Supervisor::new(desired, ids.clone());
        let tx = inputs.clone();
        std::thread::Builder::new()
            .name("uw-host".into())
            .spawn(move || run(supervisor, launch, rx, tx, etx))
            .expect("spawn the host thread");
        Host { inputs, events, ids }
    }

    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// A handle to send commands from another thread.
    pub fn handle(&self) -> HostHandle {
        HostHandle { inputs: self.inputs.clone(), ids: self.ids.clone() }
    }

    /// Send a command (it updates the desired state, then goes to the helper, or fails at once
    /// with `*_not_connected` while none runs). Its reply arrives as an event.
    pub fn send(&self, cmd: HelperCommand) {
        let _ = self.inputs.send(Input::User(cmd));
    }

    /// The next event; `None` once the host has stopped and its helper has exited.
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }

    pub fn recv_timeout(&self, d: Duration) -> Result<Event, RecvTimeoutError> {
        self.events.recv_timeout(d)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.inputs.send(Input::Shutdown);
    }
}

/// Spawn a helper and its stdout reader. The reader sends each non-empty line, then
/// `HelperExited` once stdout closes and the helper is reaped.
fn spawn(mut cmd: Command, tx: &Sender<Input>) -> Result<ChildStdin, String> {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| {
        eprintln!("[uw-host] starting the helper: {e}");
        "helper_start_failed".to_string()
    })?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let tx = tx.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            // Bytes, not `lines()`: a line that is not UTF-8 must not end the reader.
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\n', '\r']);
            if !line.trim().is_empty() && tx.send(Input::HelperLine(line.to_string())).is_err() {
                break;
            }
        }
        let _ = child.wait();
        let _ = tx.send(Input::HelperExited);
    });
    Ok(stdin)
}

fn run(mut sup: Supervisor, mut launch: impl Launch, rx: Receiver<Input>, tx: Sender<Input>, events: Sender<Event>) {
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_millis() as u64;
    let mut stdin: Option<ChildStdin> = None;
    let mut helper_running = false;
    let mut stopping = false;
    let mut actions = sup.start(now());
    loop {
        let mut i = 0;
        while i < actions.len() {
            match actions[i].clone() {
                Action::StartHelper { args } => match launch.command(&args).and_then(|cmd| spawn(cmd, &tx)) {
                    Ok(s) => {
                        stdin = Some(s);
                        helper_running = true;
                    }
                    Err(code) => actions.extend(sup.handle(Input::StartFailed(code), now())),
                },
                Action::Send(cmd) => {
                    if let Some(s) = stdin.as_mut() {
                        // A write that fails means the helper is exiting; its `HelperExited`
                        // fails the command then.
                        let _ = writeln!(s, "{}", cmd.to_line()).and_then(|_| s.flush());
                    }
                }
                Action::Emit(e) => {
                    let _ = events.send(e);
                }
                Action::Stop => {
                    stopping = true;
                    stdin = None; // EOF: the helper exits
                }
            }
            i += 1;
        }
        actions.clear();
        if stopping && !helper_running {
            return; // dropping `events` ends `Host::recv`
        }
        let input = match sup.next_deadline() {
            Some(at) => match rx.recv_timeout(Duration::from_millis(at.saturating_sub(now()))) {
                Ok(input) => Some(input),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(input) => Some(input),
                Err(_) => return,
            },
        };
        if let Some(input) = input {
            if input == Input::HelperExited {
                helper_running = false;
                stdin = None;
            }
            actions.extend(sup.handle(input, now()));
        }
        actions.extend(sup.tick(now()));
    }
}
