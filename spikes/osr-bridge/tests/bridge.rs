use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uw_spike_ipc::{InputEvent, Msg, read_msg, recv, send, write_msg};
use uw_spike_osr_bridge::{FrameInfo, PlaneFd, SendResult, Server};

const TIMEOUT: Duration = Duration::from_millis(100);

fn socket_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uw-bridge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn frame(file: &std::fs::File) -> FrameInfo {
    FrameInfo {
        widget: 1,
        x: 100,
        y: 100,
        width: 400,
        height: 300,
        pixel_format: "bgra".into(),
        modifier: "0".into(),
        planes: vec![PlaneFd { fd: file.as_raw_fd(), stride: 1792, offset: 0, size: 537_600 }],
    }
}

fn temp_file(name: &str) -> std::fs::File {
    std::fs::File::create(socket_path(name)).unwrap()
}

fn wait_until(cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(2), "condition not met in 2 s");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn send_frame_drops_without_peer() {
    let server = Server::bind(&socket_path("a.sock")).unwrap();
    let f = temp_file("a.buf");
    let start = Instant::now();
    assert_eq!(server.send_frame(&frame(&f), TIMEOUT), SendResult::Dropped);
    assert!(start.elapsed() < Duration::from_millis(5), "{:?}", start.elapsed());
}

#[test]
fn send_frame_times_out_without_peer() {
    let path = socket_path("b.sock");
    let server = Server::bind(&path).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    let f = temp_file("b.buf");

    let start = Instant::now();
    assert_eq!(server.send_frame(&frame(&f), TIMEOUT), SendResult::Timeout);
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(100) && took <= Duration::from_millis(150), "{took:?}");

    let (msg, fds) = recv(&client).unwrap();
    assert!(matches!(msg, Msg::Frame { widget: 1, width: 400, height: 300, drm_fourcc: 0x3432_5241, .. }));
    assert_eq!(fds.len(), 1);
}

#[test]
fn send_frame_drops_after_peer_disconnect() {
    let path = socket_path("c.sock");
    let server = Server::bind(&path).unwrap();
    let f = temp_file("c.buf");

    let client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    drop(client);
    wait_until(|| !server.has_peer());
    assert_eq!(server.send_frame(&frame(&f), TIMEOUT), SendResult::Dropped);

    // A new peer is accepted and acks.
    let client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    let acker = std::thread::spawn(move || {
        let (msg, _fds) = recv(&client).unwrap();
        let Msg::Frame { id, .. } = msg else { panic!("expected Frame, got {msg:?}") };
        send(&client, &Msg::Ack { id }, &[]).unwrap();
        client
    });
    assert_eq!(server.send_frame(&frame(&f), Duration::from_secs(2)), SendResult::Acked);
    drop(acker.join().unwrap());
}

#[test]
fn unsupported_pixel_format_is_dropped() {
    let path = socket_path("d.sock");
    let server = Server::bind(&path).unwrap();
    let _client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    let f = temp_file("d.buf");
    let mut fr = frame(&f);
    fr.pixel_format = "rgbaf16".into();
    assert_eq!(server.send_frame(&fr, TIMEOUT), SendResult::Dropped);
}

#[test]
fn bind_replaces_stale_socket_file() {
    let path = socket_path("e.sock");
    std::fs::write(&path, b"stale").unwrap();
    let server = Server::bind(&path).unwrap();
    let _client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
}

/// Candidate C3: the in-game Win32 hook forwards over localhost TCP. Its edit-mode toggles and
/// input must land in the same queues the host drains for the C2 path.
#[test]
fn c3_tcp_peer_feeds_edit_and_input_queues() {
    let server = Server::bind(&socket_path("g.sock")).unwrap();
    let port = 40000 + (std::process::id() % 20000) as u16;
    server.bind_input_tcp(port, Vec::new()).unwrap();
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write_msg(&mut client, &Msg::Hello { protocol: 1, pid: 1 }).unwrap();
    write_msg(&mut client, &Msg::EditMode { on: true }).unwrap();
    write_msg(&mut client, &Msg::Input(InputEvent::MouseMove { x: 3.0, y: -4.0 })).unwrap();

    wait_until(|| server.pending_edit_requests() >= 1 && server.pending_inputs() >= 1);
    assert_eq!(server.take_edit_requests(), vec![true]);
    assert_eq!(server.take_inputs(), vec![InputEvent::MouseMove { x: 3.0, y: -4.0 }]);
}

#[test]
fn c3_tcp_disconnect_while_editing_requests_edit_off() {
    let server = Server::bind(&socket_path("h.sock")).unwrap();
    let port = 40001 + (std::process::id() % 20000) as u16;
    server.bind_input_tcp(port, Vec::new()).unwrap();
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write_msg(&mut client, &Msg::EditMode { on: true }).unwrap();
    wait_until(|| server.pending_edit_requests() >= 1);
    assert_eq!(server.take_edit_requests(), vec![true]);

    drop(client);
    wait_until(|| server.pending_edit_requests() >= 1);
    assert_eq!(server.take_edit_requests(), vec![false], "a dead hook must not leave the game frozen");
}

/// The in-game module installs input hooks only when told: the host answers each module `Hello`
/// with its configured replies (`ModuleConfig` first), in order.
#[test]
fn module_hello_gets_the_configured_replies() {
    let server = Server::bind(&socket_path("i.sock")).unwrap();
    let port = 40002 + (std::process::id() % 20000) as u16;
    let replies = vec![
        Msg::ModuleConfig { input_hooks: true },
        Msg::LoadPlugin { path: r"Z:\p.dll".into(), channel: 1, args: String::new() },
    ];
    server.bind_input_tcp(port, replies.clone()).unwrap();
    for _ in 0..2 {
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_msg(&mut client, &Msg::Hello { protocol: 1, pid: 1 }).unwrap();
        for want in &replies {
            assert_eq!(&read_msg(&mut client).unwrap(), want);
        }
    }
}

#[test]
fn peer_generation_changes_on_each_connection() {
    let path = socket_path("f.sock");
    let server = Server::bind(&path).unwrap();
    assert_eq!(server.peer_generation(), 0);
    let a = UnixStream::connect(&path).unwrap();
    wait_until(|| server.peer_generation() == 1);
    drop(a);
    wait_until(|| !server.has_peer());
    assert_eq!(server.peer_generation(), 1, "disconnect alone doesn't change it");
    let _b = UnixStream::connect(&path).unwrap();
    wait_until(|| server.peer_generation() == 2);
}
