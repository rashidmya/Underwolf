use std::os::unix::net::UnixStream;

use uw_spike_ipc::{InputEvent, Msg, recv, send, window_to_swapchain};

#[test]
fn maps_window_to_swapchain_coords() {
    assert_eq!(window_to_swapchain((960.0, 540.0), (1920, 1080), (2560, 1440)), (1280.0, 720.0));
    assert_eq!(window_to_swapchain((0.0, 0.0), (1920, 1080), (1280, 720)), (0.0, 0.0));
}

#[test]
fn zero_sized_window_maps_to_origin() {
    assert_eq!(window_to_swapchain((10.0, 10.0), (0, 0), (2560, 1440)), (0.0, 0.0));
}

#[test]
fn input_and_edit_mode_roundtrip() {
    let (a, b) = UnixStream::pair().unwrap();
    let msgs = vec![
        Msg::EditMode { on: true },
        Msg::Input(InputEvent::MouseMove { x: 1.5, y: 2.5 }),
        Msg::Input(InputEvent::MouseButton { button: 0, down: true }),
        Msg::Input(InputEvent::Wheel { dx: 0.0, dy: -3.0 }),
        Msg::Input(InputEvent::Key { code: 23, down: false, text: Some("a".into()) }),
    ];
    for m in &msgs {
        send(&a, m, &[]).unwrap();
        assert_eq!(&recv(&b).unwrap().0, m);
    }
}
