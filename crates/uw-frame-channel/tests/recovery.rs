#![cfg(target_os = "linux")]
#[path = "support/actor.rs"]
mod actor;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::DirBuilderExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uw_frame_channel::*;
use uw_frame_proto::*;
static SERIAL: Mutex<()> = Mutex::new(());
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uw-frame-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Actor {
    child: Child,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    stream: UnixStream,
    _temp: Temp,
}
impl Actor {
    fn start(role: &str, scenario: &str) -> Self {
        let temp = Temp::new();
        let path = temp.0.join("actor.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "actor_entry",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("UW_CHANNEL_ACTOR_ROLE", role)
            .env("UW_CHANNEL_ACTOR_SCENARIO", scenario)
            .env("UW_CHANNEL_ACTOR_SOCKET", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        assert_ne!(child.id(), std::process::id());
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let until = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("actor exited before connect: {status}");
            }
            if Instant::now() > until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("actor connect timeout");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        Self {
            child,
            lines,
            reader: Some(reader),
            stream,
            _temp: temp,
        }
    }
    fn wait(&self, marker: &str) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = until.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .expect("actor marker timeout");
            if line.contains(marker) {
                return;
            }
        }
    }
    fn release(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
    }
    fn kill(&mut self) {
        self.child.kill().unwrap();
        assert!(!self.child.wait().unwrap().success());
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
#[test]
#[ignore = "isolated subprocess actor entry, not an acceptance case"]
fn actor_entry() {
    let role = std::env::var("UW_CHANNEL_ACTOR_ROLE").unwrap();
    let scenario = std::env::var("UW_CHANNEL_ACTOR_SCENARIO").unwrap();
    let path = PathBuf::from(std::env::var_os("UW_CHANNEL_ACTOR_SOCKET").unwrap());
    actor::run(&role, &scenario, &path).unwrap();
}
fn key(n: u64) -> AllocationKey {
    AllocationKey {
        session: SessionId([5; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 1,
        },
        generation: n,
    }
}
fn full() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 4,
        height: 2,
    }
}
fn register(actor: &Actor, n: u64) -> Arc<FrameMapping> {
    let peer = PeerIdentity::from_stream(&actor.stream).unwrap();
    let m = Arc::new(
        FrameMapping::create(key(n), FrameLayout::new(4, 2, 4096).unwrap(), peer).unwrap(),
    );
    m.try_publish(
        1,
        Patch {
            rect: full(),
            stride: 16,
            data: &[7; 32],
        },
    )
    .unwrap();
    // SAFETY: the child actor uses the same initialized native protocol.
    send_message(&actor.stream, &Message::Register(m.header()), &unsafe {
        m.descriptors()
    })
    .unwrap();
    m
}
fn receive(stream: &UnixStream) -> (Message, Vec<std::os::fd::OwnedFd>) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recv_message(stream) {
            Err(FrameError::Busy) if Instant::now() < until => continue,
            result => return result.unwrap(),
        }
    }
}
#[test]
fn another_process_keeps_pixels_owned_until_explicit_completion() {
    let _serial = SERIAL.lock().unwrap();
    let mut actor = Actor::start("consumer", "consume");
    let m = register(&actor, 1);
    actor.wait("ACTOR_READING");
    assert_eq!(
        m.try_publish(
            2,
            Patch {
                rect: full(),
                stride: 16,
                data: &[9; 32]
            }
        )
        .unwrap(),
        PublishResult::Busy
    );
    assert_eq!(m.try_state().unwrap().consumed, 0);
    actor.release();
    assert_eq!(
        receive(&actor.stream).0,
        Message::Consumed {
            key: key(1),
            consumed: 1
        }
    );
    assert_eq!(m.try_state().unwrap().consumed, 1);
    actor.wait("ACTOR_DONE");
}
#[test]
fn a_mutex_owned_in_another_process_returns_busy_before_that_owner_releases() {
    let _serial = SERIAL.lock().unwrap();
    let mut actor = Actor::start("consumer", "hold-mutex");
    let m = register(&actor, 1);
    actor.wait("ACTOR_LOCKED");
    let (tx, rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = m.try_publish(
            2,
            Patch {
                rect: full(),
                stride: 16,
                data: &[9; 32],
            },
        );
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    actor.release();
    writer.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), PublishResult::Busy);
    actor.wait("ACTOR_DONE");
}
#[test]
fn a_producer_killed_halfway_through_a_write_invalidates_instead_of_consuming() {
    let _serial = SERIAL.lock().unwrap();
    let mut actor = Actor::start("producer", "partial-write");
    let (message, fds) = receive(&actor.stream);
    let Message::Register(header) = message else {
        panic!("expected registration")
    };
    let peer = PeerIdentity::from_stream(&actor.stream).unwrap();
    // SAFETY: this native producer initializes the mutex; no reader consumes its
    // deliberately interrupted publication after robust owner-death detection.
    let m = Arc::new(
        unsafe { FrameMapping::open(&header.encode().unwrap(), fds.try_into().unwrap(), peer) }
            .unwrap(),
    );
    actor.wait("ACTOR_LOCKED");
    actor.kill();
    assert!(matches!(m.try_claim(), Err(FrameError::OwnerDied)));
    assert_eq!(m.try_state(), Err(FrameError::OwnerDied));
    let fresh = Arc::new(
        FrameMapping::create(
            key(2),
            FrameLayout::new(4, 2, 4096).unwrap(),
            PeerIdentity::current().unwrap(),
        )
        .unwrap(),
    );
    fresh
        .try_publish(
            1,
            Patch {
                rect: full(),
                stride: 16,
                data: &[9; 32],
            },
        )
        .unwrap();
    assert_eq!(
        &fresh.try_claim().unwrap().unwrap().copy_pixels().unwrap()[..16],
        &[9; 16]
    );
}
#[test]
fn cross_process_churn_releases_one_hundred_generations() {
    let _serial = SERIAL.lock().unwrap();
    let mut actor = Actor::start("consumer", "churn");
    let fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
    let mappings = live_mapping_count();
    for n in 1..=100 {
        let m = register(&actor, n);
        assert_eq!(
            receive(&actor.stream).0,
            Message::Consumed {
                key: key(n),
                consumed: 1
            }
        );
        drop(m);
    }
    actor.wait("ACTOR_DONE");
    assert_eq!(live_mapping_count(), mappings);
    assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), fds);
    actor.release();
}
