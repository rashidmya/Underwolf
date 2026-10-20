//! The transport-generic framing (`write_msg`/`read_msg`) used by the C3 Win32 hook over
//! localhost TCP. The Unix-socket `send`/`recv` path is covered by `roundtrip.rs`.

use std::io::Cursor;

use uw_spike_ipc::{InputEvent, Msg, PROTOCOL, read_msg, write_msg};

#[test]
fn write_read_roundtrip_over_a_byte_buffer() {
    let msgs = vec![
        Msg::Hello { protocol: PROTOCOL, pid: 1234 },
        Msg::EditMode { on: true },
        Msg::Input(InputEvent::MouseMove { x: 1.5, y: -2.0 }),
        Msg::Input(InputEvent::MouseButton { button: 1, down: true }),
        Msg::Input(InputEvent::Wheel { dx: 0.0, dy: 1.0 }),
        Msg::Input(InputEvent::Key { code: 0x41, down: true, text: Some("a".into()) }),
        Msg::EditMode { on: false },
    ];

    let mut buf = Vec::new();
    for m in &msgs {
        write_msg(&mut buf, m).unwrap();
    }

    let mut cur = Cursor::new(buf);
    for m in &msgs {
        assert_eq!(&read_msg(&mut cur).unwrap(), m);
    }
}

#[test]
fn read_msg_reports_eof_at_a_clean_end() {
    let mut cur = Cursor::new(Vec::new());
    assert_eq!(read_msg(&mut cur).unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
}
