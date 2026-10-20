use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uw_spike_ipc::{Msg, recv, send};
use uw_spike_osr_bridge::Server;

fn connect(name: &str) -> (Server, UnixStream) {
    let dir = std::env::temp_dir().join(format!("uw-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join(name);
    let server = Server::bind(&path).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    wait_until(|| server.has_peer());
    (server, client)
}

fn wait_until(cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(2), "condition not met in 2 s");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn edit_requests_from_layer_are_queued_in_order() {
    let (server, client) = connect("a.sock");
    send(&client, &Msg::EditMode { on: true }, &[]).unwrap();
    send(&client, &Msg::EditMode { on: false }, &[]).unwrap();
    wait_until(|| server.pending_edit_requests() == 2);
    assert_eq!(server.take_edit_requests(), vec![true, false]);
    assert!(server.take_edit_requests().is_empty());
}

#[test]
fn swapchain_size_is_reported() {
    let (server, client) = connect("b.sock");
    assert_eq!(server.swapchain_size(), None);
    send(&client, &Msg::Swapchain { width: 2560, height: 1440 }, &[]).unwrap();
    wait_until(|| server.swapchain_size().is_some());
    assert_eq!(server.swapchain_size(), Some((2560, 1440)));
}

#[test]
fn edit_mode_and_layout_reach_the_layer() {
    let (server, client) = connect("c.sock");
    assert!(server.set_edit_mode(true));
    assert!(server.set_layout(2, 300, 400, true));
    assert_eq!(recv(&client).unwrap().0, Msg::EditMode { on: true });
    assert_eq!(recv(&client).unwrap().0, Msg::Layout { widget: 2, x: 300, y: 400, visible: true });
}

#[test]
fn control_messages_without_peer_report_false() {
    let dir = std::env::temp_dir().join(format!("uw-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = Server::bind(&dir.join("d.sock")).unwrap();
    assert!(!server.set_edit_mode(true));
    assert!(!server.set_layout(0, 0, 0, true));
}

#[test]
fn input_from_layer_is_queued_in_order() {
    use uw_spike_ipc::InputEvent;
    let (server, client) = connect("e.sock");
    send(&client, &Msg::Input(InputEvent::MouseMove { x: 10.0, y: 20.0 }), &[]).unwrap();
    send(&client, &Msg::Input(InputEvent::MouseButton { button: 0, down: true }), &[]).unwrap();
    wait_until(|| server.pending_inputs() == 2);
    assert_eq!(
        server.take_inputs(),
        vec![InputEvent::MouseMove { x: 10.0, y: 20.0 }, InputEvent::MouseButton { button: 0, down: true }]
    );
    assert!(server.take_inputs().is_empty());
}
