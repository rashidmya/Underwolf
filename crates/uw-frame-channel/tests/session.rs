#![cfg(target_os = "linux")]
use std::{
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uw_frame_channel::*;
use uw_frame_proto::*;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "uw-frame-session-{}-{}",
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
fn hello(peer: PeerIdentity) -> Message {
    Message::Hello {
        pid: peer.pid,
        start_ticks: peer.start_ticks,
        alignment: 4096,
        mutex_size: 40,
        mutex_align: 8,
        abi: 1,
        capability: 1,
    }
}
fn accept(server: &mut SessionServer) -> ConsumerConnection {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match server.try_accept() {
            Ok(c) => return c,
            Err(FrameError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}
fn receive(stream: &UnixStream) -> Message {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match recv_message(stream) {
            Ok((m, _)) => return m,
            Err(FrameError::Busy) if Instant::now() < until => continue,
            Err(e) => panic!("receive: {e}"),
        }
    }
}
#[test]
fn peer_start_ticks_handle_names_with_spaces_and_parentheses() {
    let stat = format!(
        "12 (odd ) thread name) S {} 9876 0 0",
        vec!["0"; 18].join(" ")
    );
    assert_eq!(parse_start_ticks(&stat).unwrap(), 9876);
    for bad in ["", "12 (name) S 0", "no parenthesis"] {
        assert!(parse_start_ticks(bad).is_err());
    }
    let current = PeerIdentity::current().unwrap();
    assert_eq!(current, PeerIdentity::for_pid(std::process::id()).unwrap());
}
#[test]
fn a_second_client_cannot_evict_the_existing_session() {
    let root = Temp::new();
    let path = root.0.join("render.sock");
    let peer = PeerIdentity::current().unwrap();
    let mut server = SessionServer::bind_path(&path, peer).unwrap();
    assert!(matches!(
        SessionServer::bind_path(&path, peer),
        Err(FrameError::Busy)
    ));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let p = path.clone();
    let client = std::thread::spawn(move || {
        let stream = UnixStream::connect(p).unwrap();
        send_message(&stream, &hello(peer), &[]).unwrap();
        let welcome = receive(&stream);
        ready_tx.send(welcome).unwrap();
        let got = receive(&stream);
        assert_eq!(
            got,
            Message::Dimming {
                revision: 1,
                active: true
            }
        );
        release_rx.recv().unwrap();
    });
    let first = accept(&mut server);
    assert!(
        matches!(ready_rx.recv().unwrap(),Message::Welcome{session,..} if session==first.session)
    );
    let second = UnixStream::connect(&path).unwrap();
    send_message(&second, &hello(peer), &[]).unwrap();
    assert!(matches!(server.try_accept(), Err(FrameError::Busy)));
    assert!(recv_message(&second).is_err());
    send_message(
        &first.stream,
        &Message::Dimming {
            revision: 1,
            active: true,
        },
        &[],
    )
    .unwrap();
    release_tx.send(()).unwrap();
    client.join().unwrap();
    let old = first.session;
    drop(first);
    let p = path.clone();
    let client = std::thread::spawn(move || {
        let stream = UnixStream::connect(p).unwrap();
        send_message(&stream, &hello(peer), &[]).unwrap();
        receive(&stream)
    });
    let next = accept(&mut server);
    assert_ne!(next.session, old);
    assert!(matches!(client.join().unwrap(),Message::Welcome{session,..} if session==next.session));
}
#[test]
fn mismatched_peer_and_hello_identity_leave_the_endpoint_usable() {
    let root = Temp::new();
    let path = root.0.join("render.sock");
    let peer = PeerIdentity::current().unwrap();
    let mut stale = peer;
    stale.start_ticks += 1;
    assert!(SessionServer::bind_path(&path, stale).is_err());
    let mut server = SessionServer::bind_path(&path, peer).unwrap();
    let mut wrong = peer;
    wrong.pid += 1;
    let stream = UnixStream::connect(&path).unwrap();
    send_message(&stream, &hello(wrong), &[]).unwrap();
    assert!(server.try_accept().is_err());
    let p = path.clone();
    let client = std::thread::spawn(move || {
        let stream = UnixStream::connect(p).unwrap();
        send_message(&stream, &hello(peer), &[]).unwrap();
        receive(&stream)
    });
    let connection = accept(&mut server);
    assert!(
        matches!(client.join().unwrap(),Message::Welcome{session,..} if session==connection.session)
    );
    drop(connection);
    drop(server);
    assert!(!path.exists());
}

#[test]
fn stale_endpoints_can_be_recovered_but_live_foreign_sockets_are_preserved() {
    let root = Temp::new();
    let path = root.0.join("render.sock");
    let peer = PeerIdentity::current().unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(matches!(
        SessionServer::bind_path(&path, peer),
        Err(FrameError::Busy)
    ));
    assert!(path.exists());
    drop(listener);
    let server = SessionServer::bind_path(&path, peer).unwrap();
    assert!(path.exists());
    drop(server);
    assert!(!path.exists());
}
