use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uw_spike_ipc::{Msg, recv, send};
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
