#![cfg(unix)]
//! Production input DLL through the real bootstrap/helper, with per-test Wine cleanup.
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};
use uw_pipe_proto::{
    input::{CaptureCode, InputAck},
    jsonl::{Command as Cmd, Event},
};
struct Prefix {
    dir: PathBuf,
    game: Option<Child>,
}
impl Drop for Prefix {
    fn drop(&mut self) {
        if let Some(game) = self.game.as_mut() {
            let _ = game.kill();
            let _ = game.wait();
        }
        let _ = Command::new("wineserver").arg("-k").env("WINEPREFIX", &self.dir).status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn setup(tag: &str) -> Prefix {
    let dir = std::env::temp_dir().join(format!("uw-input-chain-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Prefix { dir, game: None }
}
fn win_artifact(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/x86_64-pc-windows-gnu/release").join(name);
    path.canonicalize().expect("build Windows artifacts first")
}
fn to_wine_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}
fn wine_cmd(p: &Prefix, path: &Path) -> Command {
    let mut c = Command::new("wine");
    c.arg(path).env("WINEPREFIX", &p.dir).env("WINEDEBUG", "-all").env("WINEDLLOVERRIDES", "mscoree,mshtml=");
    c
}
struct Helper {
    stdout_gate: Arc<Mutex<()>>,
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Helper {
    fn send(&mut self, line: &str) {
        writeln!(self.stdin.as_mut().unwrap(), "{line}").unwrap();
    }
    fn send_cmd(&mut self, cmd: &Cmd) {
        self.send(&cmd.to_line());
    }
    fn stderr_lines(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }
    fn next_event(&self, within: Duration) -> String {
        self.events.recv_timeout(within).unwrap_or_else(|e| panic!("{e}; {:?}", self.stderr_lines()))
    }
    fn next_parsed(&self, within: Duration) -> Event {
        Event::parse(&self.next_event(within)).unwrap()
    }
}
fn start_helper(p: &Prefix, _extra: &[&str], _envs: &[(&str, &str)]) -> Helper {
    let mut c = wine_cmd(p, &win_artifact("uw-helper64.exe"));
    let mut child = c
        .arg("uw-test-game.exe")
        .arg("--bootstrap")
        .arg(to_wine_path(&win_artifact("uw_bootstrap64.dll")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, events) = channel();
    let stdout_gate = Arc::new(Mutex::new(()));
    let gate = stdout_gate.clone();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            drop(gate.lock().unwrap());
            let Some(Ok(line)) = lines.next() else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let output = diagnostics.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            output.lock().unwrap().push(line);
        }
    });
    Helper { stdout_gate, child, stdin, events, stderr: diagnostics }
}
fn load_overlay_line(path: &str) -> String {
    Cmd::LoadOverlay { request_id: 1, path: path.into() }.to_line()
}
fn ack(helper: &mut Helper, c: uw_pipe_proto::input::InputChallenge, id: &mut u32) {
    *id += 1;
    helper.send_cmd(&Cmd::InputAck {
        request_id: *id,
        ack: InputAck { epoch: c.epoch, nonce: c.nonce, through_seq: c.through_seq },
    });
}
/// Production DLL, negotiated input, application acknowledgements, and stalled receiver release.
#[test]
#[ignore = "needs Wine and production input DLL; run under xvfb-run"]
fn production_input_capture_releases_when_receiver_stalls() {
    use uw_pipe_proto::input::{CaptureCode, InputAck, InputReason};
    let mut pfx = setup("production-input");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_helper(&pfx, &[], &[]);
    let connected = helper.next_event(Duration::from_secs(200));
    assert!(connected.contains("bootstrap_connected"), "{connected}");
    helper.send(&load_overlay_line(&to_wine_path(&win_artifact("uw_overlay.dll"))));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    let mut visible = false;
    let mut entering = false;
    let mut captured = false;
    let mut applied = false;
    let mut ack_id = 100;
    let mut acknowledged_at = Instant::now();
    loop {
        assert!(Instant::now() < deadline, "input session timed out; stderr: {:?}", helper.stderr_lines());
        let event = helper.next_parsed(deadline.saturating_duration_since(Instant::now()));
        eprintln!("input integration: {event:?}");
        match event {
            Event::InputReady { version: 1 } => ready = true,
            Event::InputState { state } => {
                if !visible {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 2,
                        input_epoch: None,
                        edit_mode: None,
                        visible: Some(true),
                    });
                    visible = true;
                }
                captured |= state.active;
                if captured && !state.active && state.reason == InputReason::LeaseExpired {
                    assert!(ready && applied);
                    assert!(acknowledged_at.elapsed() < Duration::from_secs(3));
                    break;
                }
            }
            Event::InputChallenge { challenge } if !captured => {
                ack_id += 1;
                helper.send_cmd(&Cmd::InputAck {
                    request_id: ack_id,
                    ack: InputAck {
                        epoch: challenge.epoch,
                        nonce: challenge.nonce,
                        through_seq: challenge.through_seq,
                    },
                });
                acknowledged_at = Instant::now();
                // Give the ack a full worker/owner turn before asking to capture.
                if visible && !entering && ack_id >= 103 {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 3,
                        input_epoch: Some(challenge.epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    entering = true;
                }
            }
            Event::CaptureResult { capture_request_id: 3, code, .. } => {
                assert_eq!(code, CaptureCode::Applied);
                applied = true;
            }
            Event::Error { .. } => panic!("{event:?}; stderr: {:?}", helper.stderr_lines()),
            _ => {}
        }
    }
}

#[test]
#[ignore = "needs production Windows artifacts; run under xvfb-run"]
fn helper_restart_never_recaptures_and_old_epoch_is_refused() {
    let mut pfx = setup("restart");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(60)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&to_wine_path(&win_artifact("uw_overlay.dll"))));
    let mut id = 100;
    let mut shown = false;
    let mut asked = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    let old_epoch = loop {
        match helper.next_parsed(deadline.saturating_duration_since(Instant::now())) {
            Event::InputState { state } => {
                if !shown {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 2,
                        input_epoch: None,
                        edit_mode: None,
                        visible: Some(true),
                    });
                    shown = true;
                }
                if state.active {
                    break state.epoch;
                }
            }
            Event::InputChallenge { challenge } => {
                ack(&mut helper, challenge, &mut id);
                if shown && !asked && id >= 103 {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 3,
                        input_epoch: Some(challenge.epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    asked = true;
                }
            }
            Event::Error { code, .. } => panic!("{code}"),
            _ => {}
        }
    };
    drop(helper);
    let mut helper = start_helper(&pfx, &[], &[]);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut new_epoch = 0;
    let mut challenges = 0;
    let mut stale_sent = false;
    let mut stale_refused = false;
    let mut fresh_sent = false;
    loop {
        match helper.next_parsed(deadline.saturating_duration_since(Instant::now())) {
            Event::InputState { state } => {
                new_epoch = state.epoch;
                assert!(new_epoch > old_epoch);
                if !fresh_sent {
                    assert!(!state.active, "helper restart recaptured");
                }
                if fresh_sent && state.active {
                    assert!(stale_refused);
                    break;
                }
            }
            Event::InputChallenge { challenge } => {
                ack(&mut helper, challenge, &mut id);
                challenges += 1;
                if new_epoch != 0 && !stale_sent {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 4,
                        input_epoch: Some(old_epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    stale_sent = true;
                }
                // Stay idle for >500 ms with a healthy receiver before explicit recapture.
                if challenges >= 8 && stale_refused && !fresh_sent {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 5,
                        input_epoch: Some(challenge.epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    fresh_sent = true;
                }
            }
            Event::Error { request_id: Some(4), code, .. } => {
                assert_eq!(code, "input_stale_epoch");
                stale_refused = true;
            }
            Event::Error { code, .. } => panic!("{code}"),
            Event::CaptureResult { capture_request_id: 5, code, .. } => assert_eq!(code, CaptureCode::Applied),
            _ => {}
        }
    }
}

#[test]
#[ignore = "needs production DLL, test game and input_sender; run under xvfb-run"]
fn receiver_stall_releases_even_when_helper_stdout_blocks() {
    let mut pfx = setup("backpressure");
    let log = pfx.dir.join("input.log");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_INPUT_LOG", to_wine_path(&log)).spawn().unwrap(),
    );
    let mut helper = start_helper(&pfx, &[], &[]);
    assert!(helper.next_event(Duration::from_secs(60)).contains("bootstrap_connected"));
    helper.send(&load_overlay_line(&to_wine_path(&win_artifact("uw_overlay.dll"))));
    let mut id = 100;
    let mut shown = false;
    let mut asked = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match helper.next_parsed(deadline.saturating_duration_since(Instant::now())) {
            Event::InputState { state } => {
                if !shown {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 2,
                        input_epoch: None,
                        edit_mode: None,
                        visible: Some(true),
                    });
                    shown = true;
                }
                if state.active {
                    break;
                }
            }
            Event::InputChallenge { challenge } => {
                ack(&mut helper, challenge, &mut id);
                if shown && !asked && id >= 103 {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 3,
                        input_epoch: Some(challenge.epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    asked = true;
                }
            }
            Event::Error { code, .. } => panic!("{code}"),
            _ => {}
        }
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
    let gate = helper.stdout_gate.clone();
    let paused = gate.lock().unwrap();
    let mut sender = wine_cmd(&pfx, &win_artifact("examples/input_sender.exe")).spawn().unwrap();
    let start = Instant::now();
    let mut blocked = false;
    let mut resumed = false;
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{}/task", helper.child.id())) {
            blocked |= tasks
                .flatten()
                .any(|task| std::fs::read_to_string(task.path().join("wchan")).is_ok_and(|s| s.contains("pipe_write")));
        }
        if std::fs::metadata(&log).is_ok_and(|m| m.len() > 0) {
            resumed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let elapsed = start.elapsed();
    let _ = sender.kill();
    let _ = sender.wait();
    assert!(blocked, "helper stdout never reached real pipe backpressure");
    assert!(resumed, "game input did not resume while receiver stdout was blocked");
    assert!(elapsed < Duration::from_millis(1000), "release took {elapsed:?}");
    assert!(helper.child.try_wait().unwrap().is_none(), "helper exited instead of remaining blocked");
    drop(paused);
}

fn visibility_commands(separate: bool) {
    use uw_pipe_proto::input::{CaptureCode, InputAck};
    let mut pfx = setup(if separate { "separate-hide" } else { "combined-hide" });
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_helper(&pfx, &[], &[]);
    let connected = helper.next_event(Duration::from_secs(200));
    assert!(connected.contains("bootstrap_connected"), "{connected}");
    helper.send(&load_overlay_line(&to_wine_path(&win_artifact("uw_overlay.dll"))));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    let mut visible = false;
    let mut entering = false;
    let mut captured = false;
    let mut applied = false;
    let mut ack_id = 100;
    let mut capture_seq = None;
    let mut bad_ack = false;
    let mut pong = false;
    let mut off_seq = None;
    let mut off_result = false;
    let mut off_state = false;
    let mut off_bell = false;
    loop {
        assert!(Instant::now() < deadline, "input session timed out; stderr: {:?}", helper.stderr_lines());
        let event = helper.next_parsed(deadline.saturating_duration_since(Instant::now()));
        eprintln!("input integration: {event:?}");
        match event {
            Event::StateSet { request_id: 4, seq } => {
                capture_seq = Some(seq);
                if !separate {
                    off_seq = Some(seq);
                }
            }
            Event::StateSet { request_id: 5, seq } => off_seq = Some(seq),
            Event::Error { request_id: Some(700), code, .. } => {
                assert_eq!(code, "input_bad_ack");
                bad_ack = true;
            }
            Event::Pong { request_id: 701, .. } => pong = true,
            Event::CaptureResult { capture_request_id: 4, state_seq, code, .. } => {
                assert_eq!(code, CaptureCode::Applied);
                assert_eq!(Some(state_seq), capture_seq);
                off_result = true;
            }
            Event::Bell { applied_seq, .. } if off_seq == Some(applied_seq) => off_bell = true,
            Event::InputReady { version: 1 } => ready = true,
            Event::InputState { state } => {
                if !visible {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 2,
                        input_epoch: None,
                        edit_mode: None,
                        visible: Some(true),
                    });
                    visible = true;
                }
                captured |= state.active;
                if off_seq.is_some() && !state.active && state.state_seq == off_seq {
                    off_state = true;
                }
            }
            Event::InputChallenge { challenge } => {
                if ack_id == 100 {
                    helper.send_cmd(&Cmd::InputAck {
                        request_id: 700,
                        ack: InputAck { epoch: challenge.epoch, nonce: 0, through_seq: 0 },
                    });
                    helper.send_cmd(&Cmd::Ping { request_id: 701 });
                }
                ack_id += 1;
                helper.send_cmd(&Cmd::InputAck {
                    request_id: ack_id,
                    ack: InputAck {
                        epoch: challenge.epoch,
                        nonce: challenge.nonce,
                        through_seq: challenge.through_seq,
                    },
                });

                // Give the ack a full worker/owner turn before asking to capture.
                if visible && !entering && ack_id >= 103 {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 3,
                        input_epoch: Some(challenge.epoch),
                        edit_mode: Some(true),
                        visible: None,
                    });
                    entering = true;
                }
            }
            Event::CaptureResult { capture_request_id: 3, epoch, code, .. } => {
                assert_eq!(code, CaptureCode::Applied);
                applied = true;
                helper.send_cmd(&Cmd::SetState {
                    request_id: 4,
                    input_epoch: Some(epoch),
                    edit_mode: Some(false),
                    visible: if separate { None } else { Some(false) },
                });
                if separate {
                    helper.send_cmd(&Cmd::SetState {
                        request_id: 5,
                        input_epoch: None,
                        edit_mode: None,
                        visible: Some(false),
                    });
                }
            }
            Event::Error { .. } => panic!("{event:?}; stderr: {:?}", helper.stderr_lines()),
            _ => {}
        }
        if off_result && off_state && off_bell {
            assert!(ready && applied && captured && bad_ack && pong);
            break;
        }
    }
}

#[test]
#[ignore = "needs production Windows artifacts; run under xvfb-run"]
fn hide_and_edit_off_acknowledge_the_same_applied_sequence() {
    visibility_commands(false);
}
#[test]
#[ignore = "needs production Windows artifacts; run under xvfb-run"]
fn separate_edit_and_hide_preserve_visibility_and_reject_malformed_ack() {
    visibility_commands(true);
}
