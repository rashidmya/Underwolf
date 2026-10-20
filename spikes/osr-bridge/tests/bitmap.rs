use std::fs::File;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uw_spike_ipc::{Msg, recv, send};
use uw_spike_osr_bridge::{Bitmap, SendResult, Server};

fn socket_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uw-bitmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn wait_until(cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(2), "condition not met in 2 s");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// 4×2 BGRA pixels, stride 16.
fn pixels() -> Vec<u8> {
    (0u8..32).collect()
}

fn bitmap(data: &[u8]) -> Bitmap<'_> {
    Bitmap { widget: 7, x: 100, y: 450, width: 4, height: 2, stride: 16, pixel_format: "bgra", data }
}

fn connect(name: &str) -> (Server, UnixStream) {
    let path = socket_path(name);
    let server = Server::bind(&path).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    (server, client)
}

#[test]
fn send_bitmap_drops_without_peer() {
    let server = Server::bind(&socket_path("a.sock")).unwrap();
    let data = pixels();
    let start = Instant::now();
    assert_eq!(server.send_bitmap(&bitmap(&data), Duration::from_millis(100)), SendResult::Dropped);
    assert!(start.elapsed() < Duration::from_millis(5));
}

#[test]
fn send_bitmap_registers_slot_once_per_peer() {
    let (server, client) = connect("b.sock");
    let data = pixels();
    let peer = std::thread::spawn(move || {
        let mut seen = Vec::new();
        while seen.len() < 3 {
            let (msg, fds) = recv(&client).unwrap();
            match &msg {
                Msg::Slot { .. } => assert_eq!(fds.len(), 1),
                Msg::FrameShm { id, .. } => send(&client, &Msg::Ack { id: *id }, &[]).unwrap(),
                other => panic!("unexpected {other:?}"),
            }
            seen.push(msg);
        }
        seen
    });
    for _ in 0..2 {
        assert_eq!(server.send_bitmap(&bitmap(&data), Duration::from_secs(2)), SendResult::Acked);
    }
    let seen = peer.join().unwrap();
    assert!(matches!(
        seen[0],
        Msg::Slot { widget: 7, slot: 0, width: 4, height: 2, stride: 16, drm_fourcc: 0x3432_5241 }
    ));
    assert!(matches!(seen[1], Msg::FrameShm { widget: 7, slot: 0, x: 100, y: 450, .. }));
    assert!(matches!(seen[2], Msg::FrameShm { widget: 7, slot: 0, .. }));
}

#[test]
fn send_bitmap_memfd_contains_pixels() {
    let (server, client) = connect("c.sock");
    let data = pixels();
    let expected = data.clone();
    let peer = std::thread::spawn(move || {
        let (msg, mut fds) = recv(&client).unwrap();
        assert!(matches!(msg, Msg::Slot { .. }), "{msg:?}");
        let file = File::from(fds.remove(0));
        let (msg, _) = recv(&client).unwrap();
        let Msg::FrameShm { id, .. } = msg else { panic!("{msg:?}") };
        let mut got = vec![0u8; 32];
        file.read_exact_at(&mut got, 0).unwrap();
        send(&client, &Msg::Ack { id }, &[]).unwrap();
        got
    });
    assert_eq!(server.send_bitmap(&bitmap(&data), Duration::from_secs(2)), SendResult::Acked);
    assert_eq!(peer.join().unwrap(), expected);
}

#[test]
fn send_bitmap_drops_when_all_slots_busy() {
    let (server, client) = connect("d.sock");
    let data = pixels();
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| assert_eq!(server.send_bitmap(&bitmap(&data), Duration::from_millis(500)), SendResult::Timeout));
        }
        // Wait until the peer has received all three frames (none acked).
        let mut frames = 0;
        while frames < 3 {
            if let (Msg::FrameShm { .. }, _) = recv(&client).unwrap() {
                frames += 1;
            }
        }
        let start = Instant::now();
        assert_eq!(server.send_bitmap(&bitmap(&data), Duration::from_millis(500)), SendResult::Dropped);
        assert!(start.elapsed() < Duration::from_millis(5), "{:?}", start.elapsed());
    });
}
