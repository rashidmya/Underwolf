#![cfg(unix)]
//! Real v2 CLI → host → helper → bootstrap → production DLL recovery.
//! Each fixture owns a private prefix; no owner game is selected or changed.
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};
use uw_host::routing::{RoutingReceiver, RoutingReceiverAction};
use uw_pipe_proto::{
    input::InputReason,
    jsonl::{Command as Cmd, Event, RoutingCommand, RoutingEvent},
    routing::*,
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}
fn win(name: &str) -> PathBuf {
    root().join("target/x86_64-pc-windows-gnu/release").join(name).canonicalize().expect("build native fixtures")
}
struct Desktop {
    prefix: PathBuf,
    game: Child,
    pid: i32,
}
impl Desktop {
    fn new(tag: &str) -> Self {
        let prefix = std::env::temp_dir().join(format!("uw-routing-chain-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&prefix).unwrap();
        let mut command = Command::new("wine");
        command
            .arg(win("uw-test-game.exe"))
            .env("WINEPREFIX", &prefix)
            .env("WINEDEBUG", "-all")
            .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
            .env("UW_TEST_ROUTING_CONTROLS", "1")
            .env("UW_TEST_INPUT_LOG", format!("Z:{}", prefix.join("input.log").to_string_lossy().replace('/', "\\")))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut desktop = Self { prefix, game: command.spawn().unwrap(), pid: 0 };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(pid) = desktop.process("uw-test-game.exe") {
                desktop.pid = pid;
                return desktop;
            }
            assert!(Instant::now() < deadline, "private test game never started");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn process(&self, name: &str) -> Option<i32> {
        let prefix = format!("WINEPREFIX={}", self.prefix.display());
        uw_host::discovery::find_pids(name).into_iter().find(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|byte| *byte == 0).any(|entry| entry == prefix.as_bytes()))
        })
    }
    fn sender(&self) -> Sender {
        self.controller(&[])
    }
    fn controller(&self, arguments: &[&str]) -> Sender {
        Sender(
            Command::new("wine")
                .arg(win("examples/input_sender.exe"))
                .args(arguments)
                .env("WINEPREFIX", &self.prefix)
                .env("WINEDEBUG", "-all")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.game.kill();
        let _ = self.game.wait();
        let _ = Command::new("wineserver").arg("-k").env("WINEPREFIX", &self.prefix).status();
        let _ = std::fs::remove_dir_all(&self.prefix);
    }
}
struct Sender(Child);
impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Host {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<String>,
    receiver: RoutingReceiver,
    state: Option<RoutingState>,
    published: bool,
    applied: bool,
    id: u32,
    paused: bool,
    withheld: usize,
    consumed_targets: HashSet<WidgetRefKey>,
    held: HashSet<(u32, u32, u8, i32)>,
    live_generation: u32,
}
type WidgetRefKey = (u32, u32);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    fn new(desktop: &Desktop) -> Self {
        let mut child = Command::new(root().join("target/release/uw-inject"))
            .args(["--exe", "uw-test-game.exe", "--pid", &desktop.pid.to_string(), "--proton", "/usr", "--helper"])
            .arg(win("uw-helper64.exe"))
            .arg("--bootstrap")
            .arg(win("uw_bootstrap64.dll"))
            .arg("--overlay")
            .arg(win("uw_overlay.dll"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("build host release injector");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            events,
            receiver: RoutingReceiver::default(),
            state: None,
            published: false,
            applied: false,
            id: 1000,
            paused: false,
            withheld: 0,
            consumed_targets: HashSet::new(),
            held: HashSet::new(),
            live_generation: 1,
        }
    }
    fn next_id(&mut self) -> u32 {
        self.id = self.id.checked_add(1).unwrap();
        self.id
    }
    fn send(&mut self, command: Cmd) {
        writeln!(self.stdin, "{}", command.to_line()).unwrap();
    }
    fn config(&mut self, generation: u32) -> RoutingConfig {
        let state = self.state.unwrap();
        RoutingConfig {
            epoch: state.epoch,
            revision: state.revision.checked_add(1).unwrap(),
            request_id: self.next_id(),
            width: state.width.max(1),
            height: state.height.max(1),
            widgets: vec![WidgetSpec {
                instance: WidgetRef { id: 2, generation },
                x: 0,
                y: 0,
                width: state.width.max(1) as i32,
                height: state.height.max(1) as i32,
                z: 1,
                mode: InputMode::Block,
                visible: true,
                keyboard: true,
            }],
            hotkeys: vec![HotkeySpec {
                id: 1,
                vk: 9,
                modifiers: 8,
                flags: 1,
                action: HotkeyAction::ToggleEdit,
                target: None,
            }],
        }
    }
    fn read(&mut self, within: Duration) -> Event {
        let line = self.events.recv_timeout(within).expect("production routing host stopped responding");
        let event = Event::parse(&line).unwrap();
        eprintln!("routing chain: {line}");
        match self.receiver.event(&event).expect("invalid routing receiver delivery") {
            RoutingReceiverAction::Reset(state) => {
                self.state = Some(state);
                self.applied = false;
                self.withheld = 0;
                self.consumed_targets.clear();
                self.held.clear();
            }
            RoutingReceiverAction::State(state) => self.state = Some(state),
            RoutingReceiverAction::Records(batch) => {
                if self.paused {
                    self.withheld += batch.events.len();
                } else {
                    for record in &batch.events {
                        if let Some(target) = record.target {
                            if record.kind == EventKind::Cancel {
                                self.held.retain(|(id, generation, _, _)| {
                                    (*id, *generation) != (target.id, target.generation)
                                });
                            }
                            if let Some(input) = record.input
                                && matches!(input.kind, 1 | 4)
                            {
                                let value = (target.id, target.generation, input.kind, input.a);
                                if input.flags & 1 != 0 {
                                    self.held.insert(value);
                                } else {
                                    self.held.remove(&value);
                                }
                            }
                        }
                        if record.kind == EventKind::Input {
                            let target = record.target.expect("targeted native input");
                            assert_eq!(
                                target.generation, self.live_generation,
                                "stale input reached a replaced widget"
                            );
                            self.consumed_targets.insert((target.id, target.generation));
                        }
                    }
                    self.receiver.consumed(batch.first_seq + batch.events.len() as u64 - 1).unwrap();
                }
            }
            RoutingReceiverAction::Challenge(challenge) => {
                if let Some(ack) = self.receiver.acknowledge(challenge) {
                    let request_id = self.next_id();
                    self.send(Cmd::Routing(RoutingCommand::RoutingAck { request_id, ack: ack.into() }));
                }
            }
            RoutingReceiverAction::Configured(result) if result.code == RoutingCode::Applied => self.applied = true,
            _ => {}
        }
        if !self.published && self.state.is_some_and(|state| state.width != 0 && state.height != 0) {
            self.published = true;
            let config = self.config(1);
            self.send(Cmd::Routing(RoutingCommand::ConfigureInput { request_id: config.request_id, config }));
        }
        event
    }
    fn until(&mut self, mut done: impl FnMut(&Self, &Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let event = self.read(deadline.saturating_duration_since(Instant::now()));
            if done(self, &event) {
                return event;
            }
            assert!(Instant::now() < deadline, "routing chain deadline");
        }
    }
    fn request(&mut self, action: RoutingAction, target: Option<WidgetRef>) -> u32 {
        let state = self.state.unwrap();
        let request_id = self.next_id();
        let request = RoutingRequest {
            epoch: state.epoch,
            revision: state.revision,
            request_id,
            target,
            action,
            state_seq: None,
        };
        self.send(Cmd::Routing(RoutingCommand::RouteRequest { request_id, request }));
        request_id
    }
    fn capture(&mut self) {
        self.capture_instance(1);
    }
    fn capture_instance(&mut self, generation: u32) {
        self.until(|host, event| {
            host.applied && matches!(event, Event::Routing(RoutingEvent::RoutingChallenge { .. }))
        });
        // One fresh heartbeat round-trip is processed before entry.
        self.until(|_, event| matches!(event, Event::Routing(RoutingEvent::RoutingAcked { .. })));
        let id = self.request(RoutingAction::EnterEdit, None);
        self.until(|host, event| matches!(event, Event::Routing(RoutingEvent::RoutingResult { result }) if result.request_id == id && result.code == RoutingCode::Applied) && host.state.unwrap().exclusive);
        let id = self.request(RoutingAction::Takeover, Some(WidgetRef { id: 2, generation }));
        self.until(|host, event| matches!(event, Event::Routing(RoutingEvent::RoutingResult { result }) if result.request_id == id && result.code == RoutingCode::Applied) && host.state.unwrap().focus == Some(WidgetRef { id: 2, generation }));
    }
    fn idle(&mut self, milliseconds: u64) {
        let deadline = Instant::now() + Duration::from_millis(milliseconds);
        while Instant::now() < deadline {
            self.read(Duration::from_secs(2));
            let state = self.state.unwrap();
            assert!(
                !state.exclusive && state.domains.bits() == 0 && state.focus.is_none() && state.takeover.is_none(),
                "recovery replayed ownership: {state:?}"
            );
        }
    }
}
#[test]
#[ignore = "production Windows artifacts and release uw-inject under xvfb-run"]
fn healthy_control_with_unconsumed_target_expires_and_resume_does_not_recapture() {
    let desktop = Desktop::new("target-stall");
    let mut host = Host::new(&desktop);
    host.capture();
    let epoch = host.state.unwrap().epoch;
    host.paused = true;
    let _sender = desktop.controller(&["--paced"]);
    let mut withheld = 0;
    host.until(|host, event| {
        withheld = withheld.max(host.withheld);
        matches!(event, Event::Routing(RoutingEvent::RoutingState { state }) if state.epoch > epoch && state.reason == InputReason::LeaseExpired && !state.exclusive)
    });
    assert!(withheld > 0, "no actual targeted input was withheld");
    host.paused = false;
    host.idle(600);
    host.capture();
}
#[test]
#[ignore = "production Windows artifacts and release uw-inject under xvfb-run"]
fn helper_kill_with_queued_configuration_republishes_only_table_and_rejects_old_epoch() {
    let desktop = Desktop::new("helper-loss");
    let mut host = Host::new(&desktop);
    host.capture();
    let old = host.state.unwrap();
    let config = host.config(1);
    host.send(Cmd::Routing(RoutingCommand::ConfigureInput { request_id: config.request_id, config }));
    let helper = desktop.process("uw-helper64.exe").expect("own prefix helper");
    assert!(Command::new("kill").args(["-KILL", &helper.to_string()]).status().unwrap().success());
    host.until(|host, event| {
        host.state.is_some_and(|state| state.epoch > old.epoch)
            && host.applied
            && matches!(event, Event::Routing(RoutingEvent::RoutingChallenge { .. }))
    });
    host.idle(500);
    let request_id = host.next_id();
    host.send(Cmd::Routing(RoutingCommand::RouteRequest {
        request_id,
        request: RoutingRequest {
            epoch: old.epoch,
            revision: old.revision,
            request_id,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        },
    }));
    host.until(|_, event| matches!(event, Event::Error { request_id: Some(id), code, .. } if *id == request_id && code == "routing_stale_epoch"));
    host.idle(200);
    host.capture();
}
#[test]
#[ignore = "production Windows artifacts and release uw-inject under xvfb-run"]
fn receiver_process_disconnect_reconnects_without_focus_or_input_replay() {
    let desktop = Desktop::new("client-loss");
    let mut first = Host::new(&desktop);
    first.capture();
    let epoch = first.state.unwrap().epoch;
    let sender = desktop.sender();
    first.until(|host, _| host.consumed_targets.contains(&(2, 1)));
    drop(first);
    drop(sender);
    let mut second = Host::new(&desktop);
    second.until(|host, _| host.state.is_some_and(|state| state.epoch > epoch) && host.applied);
    second.idle(700);
    assert!(second.consumed_targets.is_empty(), "previous client input reached the new receiver");
    second.capture();
}
#[test]
#[ignore = "production Windows artifacts and release uw-inject under xvfb-run"]
fn held_input_is_cancelled_before_reusing_a_widget_generation() {
    let desktop = Desktop::new("generation");
    let mut host = Host::new(&desktop);
    host.capture();
    let mut pulse = desktop.controller(&["--held-pulse"]);
    host.until(|host, _| host.held.contains(&(2, 1, 1, 65)));
    let config = host.config(2);
    let request_id = config.request_id;
    host.send(Cmd::Routing(RoutingCommand::ConfigureInput { request_id, config }));
    let mut cancelled = false;
    host.until(|_, event| {
        if let Event::Routing(RoutingEvent::RoutedBatch { batch }) = event {
            cancelled |= batch.events.iter().any(|record| record.kind == EventKind::Cancel && record.target == Some(WidgetRef { id: 2, generation: 1 }));
        }
        matches!(event, Event::Routing(RoutingEvent::RoutingConfigured { result }) if result.request_id == request_id && result.code == RoutingCode::Applied)
    });
    assert!(cancelled, "configuration retired before old target cancellation");
    assert!(host.held.is_empty(), "old held state survived generation reuse");
    host.live_generation = 2;
    while pulse.0.try_wait().unwrap().is_none() {
        host.read(Duration::from_secs(2));
    }
    host.capture_instance(2);
    let _sender = desktop.sender();
    host.until(|host, _| host.consumed_targets.contains(&(2, 2)));
}
#[test]
#[ignore = "production Windows artifacts and release uw-inject under xvfb-run"]
fn hide_focus_replacement_and_optional_loading_never_replay_ownership() {
    let desktop = Desktop::new("boundaries");
    let mut host = Host::new(&desktop);
    host.capture();
    let epoch = host.state.unwrap().epoch;
    let request_id = host.next_id();
    host.send(Cmd::SetState { request_id, input_epoch: None, edit_mode: None, visible: Some(false) });
    host.until(|host, _| host.state.is_some_and(|state| state.epoch > epoch && !state.exclusive));
    host.idle(200);
    let request_id = host.next_id();
    host.send(Cmd::SetState { request_id, input_epoch: None, edit_mode: None, visible: Some(true) });
    host.idle(400);
    for (control, marker) in
        [("1", "control focus-restored"), ("2", "control replacement"), ("3", "control optional-loaded")]
    {
        host.capture();
        let epoch = host.state.unwrap().epoch;
        let mut controller = desktop.controller(&["--control", control]);
        host.until(|host, _| host.state.is_some_and(|state| state.epoch > epoch && !state.exclusive));
        host.idle(650);
        let trace = std::fs::read_to_string(desktop.prefix.join("input.log")).unwrap();
        assert!(trace.contains(marker), "native boundary control did not execute: {trace}");
        assert!(controller.0.wait().unwrap().success());
    }
    host.capture();
}
