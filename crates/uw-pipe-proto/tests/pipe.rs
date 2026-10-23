use uw_pipe_proto::pipe::{
    decode, encode, Message, ProtoError, StatusCode, MAX_PATH_UNITS, MAX_PAYLOAD,
    MAX_STATUS_BYTES, PROTOCOL_VERSION,
};

fn roundtrip(m: &Message) {
    let bytes = encode(m);
    assert_eq!(&decode(&bytes).unwrap(), m, "round trip");
}

#[test]
fn hello_round_trips() {
    roundtrip(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 7,
        bitness: 64,
        overlay_loaded: true,
        overlay_path: r"Z:\games\overlay.dll".into(),
    });
    roundtrip(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 7,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
}

#[test]
fn load_overlay_round_trips() {
    roundtrip(&Message::LoadOverlay { request_id: 42, path: r"Z:\a\b\overlay.dll".into() });
}

#[test]
fn status_round_trips() {
    roundtrip(&Message::Status {
        request_id: 42,
        code: StatusCode::Ok,
        detail: 0,
        message: "started".into(),
    });
    roundtrip(&Message::Status {
        request_id: 1,
        code: StatusCode::LoadFailed,
        detail: 126,
        message: "LoadLibraryW failed".into(),
    });
}

#[test]
fn bad_magic_is_rejected() {
    let mut bytes = encode(&Message::Status {
        request_id: 1,
        code: StatusCode::Ok,
        detail: 0,
        message: String::new(),
    });
    bytes[0] = b'X';
    assert!(matches!(decode(&bytes), Err(ProtoError::BadMagic)));
}

#[test]
fn oversize_payload_is_rejected() {
    // A header claiming more than MAX_PAYLOAD payload bytes.
    let mut header = Vec::new();
    header.extend_from_slice(b"UWP1");
    header.extend_from_slice(&3u16.to_le_bytes()); // type = status
    header.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes()); // pid
    header.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
    assert!(matches!(decode(&header), Err(ProtoError::PayloadTooLarge)));
}

#[test]
fn overlong_path_length_prefix_is_rejected() {
    let long = "A".repeat(MAX_PATH_UNITS + 1);
    // decode: a load_overlay whose path length prefix exceeds the cap (encode caps by truncating).
    let mut payload = Vec::new();
    payload.extend_from_slice(&7u32.to_le_bytes()); // request id
    payload.extend_from_slice(&((MAX_PATH_UNITS + 1) as u16).to_le_bytes());
    for u in long.encode_utf16() {
        payload.extend_from_slice(&u.to_le_bytes());
    }
    let bytes = frame(2, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::StringTooLong)));
}

#[test]
fn overlong_status_message_is_rejected() {
    let payload_message = "x".repeat(MAX_STATUS_BYTES + 1);
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // request id
    payload.extend_from_slice(&0u16.to_le_bytes()); // code = ok
    payload.extend_from_slice(&0u32.to_le_bytes()); // detail
    payload.extend_from_slice(&((MAX_STATUS_BYTES + 1) as u16).to_le_bytes());
    payload.extend_from_slice(payload_message.as_bytes());
    let bytes = frame(3, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::StringTooLong)));
}

#[test]
fn unknown_type_is_reported() {
    let bytes = frame(999, &[]);
    assert!(matches!(decode(&bytes), Err(ProtoError::UnknownType(999))));
}

#[test]
fn wrong_header_version_is_rejected() {
    let mut bytes = encode(&Message::Status {
        request_id: 1,
        code: StatusCode::Ok,
        detail: 0,
        message: String::new(),
    });
    bytes[6] = 9; // header version (bytes 6..8) -> 9
    assert!(matches!(decode(&bytes), Err(ProtoError::BadVersion(9))));
}

#[test]
fn unknown_status_code_is_bad_code() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // request id
    payload.extend_from_slice(&99u16.to_le_bytes()); // code = 99 (not a StatusCode)
    payload.extend_from_slice(&0u32.to_le_bytes()); // detail
    payload.extend_from_slice(&0u16.to_le_bytes()); // empty message
    let bytes = frame(3, &payload);
    assert!(matches!(decode(&bytes), Err(ProtoError::BadCode(99))));
}

#[test]
fn short_buffer_is_rejected() {
    assert!(matches!(decode(&[0u8; 15]), Err(ProtoError::Short)));
    assert!(matches!(decode(&[]), Err(ProtoError::Short)));
}

#[test]
fn truncated_payloads_are_rejected() {
    // Header fine, payload too short for its fields.
    assert!(matches!(decode(&frame(3, &[])), Err(ProtoError::Truncated)));
    assert!(matches!(decode(&frame(1, &[1, 0, 7, 0])), Err(ProtoError::Truncated)));
    // Header claims 8 payload bytes but only 2 follow.
    let mut bytes = frame(3, &[]);
    bytes[12..16].copy_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    assert!(matches!(decode(&bytes), Err(ProtoError::Truncated)));
}

#[test]
fn invalid_strings_are_rejected() {
    // load_overlay whose path is a lone high surrogate.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&0xD800u16.to_le_bytes());
    assert!(matches!(decode(&frame(2, &payload)), Err(ProtoError::BadString)));
    // status whose message is invalid UTF-8.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&2u16.to_le_bytes());
    payload.extend_from_slice(&[0xff, 0xfe]);
    assert!(matches!(decode(&frame(3, &payload)), Err(ProtoError::BadString)));
}

#[test]
fn encode_truncates_to_the_caps() {
    let Message::LoadOverlay { path, .. } =
        decode(&encode(&Message::LoadOverlay { request_id: 1, path: "A".repeat(2000) })).unwrap()
    else {
        panic!("not load_overlay")
    };
    assert_eq!(path.encode_utf16().count(), MAX_PATH_UNITS);

    let status = |message: String| match decode(&encode(&Message::Status {
        request_id: 1,
        code: StatusCode::Ok,
        detail: 0,
        message,
    }))
    .unwrap()
    {
        Message::Status { message, .. } => message,
        _ => panic!("not status"),
    };
    // 255 x 'x' then a 2-byte char: the cap lands mid code point, so back off to 255 bytes.
    assert_eq!(status(format!("{}é", "x".repeat(255))).len(), 255);
    // 200 x 2-byte chars = 400 bytes: truncated to exactly 256 bytes.
    assert_eq!(status("é".repeat(200)).len(), MAX_STATUS_BYTES);
}

/// Build a frame with a valid magic and the given type over a raw payload.
fn frame(ty: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"UWP1");
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

// --- The overlay pipe (plan 2a) ---

use uw_pipe_proto::pipe::{check_overlay_first, decode_overlay, encode_overlay, OverlayFirst, OverlayMessage};

#[test]
fn overlay_messages_round_trip() {
    for m in [
        OverlayMessage::Hello { module_version: 3, state_layout: 1 },
        OverlayMessage::Ping { nonce: 77 },
        OverlayMessage::Pong { nonce: 77, seen_seq: u64::MAX - 1 },
    ] {
        assert_eq!(decode_overlay(&encode_overlay(&m)).unwrap(), m);
    }
}

/// Catches the payload layout drifting from the design table (hello 8 bytes, ping 4, pong 16).
#[test]
fn overlay_payloads_have_the_design_sizes_and_types() {
    let hello = encode_overlay(&OverlayMessage::Hello { module_version: 3, state_layout: 1 });
    assert_eq!((u16::from_le_bytes([hello[4], hello[5]]), hello.len() - 16), (0x0101, 8));
    let ping = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    assert_eq!((u16::from_le_bytes([ping[4], ping[5]]), ping.len() - 16), (0x0102, 4));
    let pong = encode_overlay(&OverlayMessage::Pong { nonce: 1, seen_seq: 9 });
    assert_eq!((u16::from_le_bytes([pong[4], pong[5]]), pong.len() - 16), (0x0103, 16));
    assert_eq!(u64::from_le_bytes(pong[24..32].try_into().unwrap()), 9, "seen_seq at payload offset 8");
}

/// Catches one pipe accepting the other pipe's messages (design: types from 0x0100 are rejected
/// on the bootstrap pipe, and the bootstrap's on the overlay pipe).
#[test]
fn a_message_from_the_other_pipe_is_an_unknown_type() {
    let status = encode(&Message::Status { request_id: 1, code: StatusCode::Ok, detail: 0, message: String::new() });
    assert!(matches!(decode_overlay(&status), Err(ProtoError::UnknownType(3))));
    let ping = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    assert!(matches!(decode(&ping), Err(ProtoError::UnknownType(0x0102))));
}

#[test]
fn short_bad_magic_and_truncated_overlay_messages_are_rejected() {
    assert!(matches!(decode_overlay(&[0u8; 15]), Err(ProtoError::Short)));
    let mut bytes = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    bytes[0] = b'X';
    assert!(matches!(decode_overlay(&bytes), Err(ProtoError::BadMagic)));
    // A pong whose payload stops after the nonce.
    assert!(matches!(decode_overlay(&frame(0x0103, &1u32.to_le_bytes())), Err(ProtoError::Truncated)));
}

/// Catches the helper accepting a first message that is not a supported `overlay_hello`.
#[test]
fn the_first_overlay_message_must_be_a_supported_hello() {
    let hello = |layout| encode_overlay(&OverlayMessage::Hello { module_version: 5, state_layout: layout });
    assert_eq!(check_overlay_first(&hello(1)), OverlayFirst::Hello { module_version: 5 });
    // Another state layout, or another header version: version_mismatch, not pipe_error.
    assert_eq!(check_overlay_first(&hello(2)), OverlayFirst::VersionMismatch);
    let mut newer = hello(1);
    newer[6..8].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(check_overlay_first(&newer), OverlayFirst::VersionMismatch);
    // A ping, a bootstrap hello, or garbage first: pipe_error.
    assert!(matches!(check_overlay_first(&encode_overlay(&OverlayMessage::Ping { nonce: 1 })), OverlayFirst::Bad(_)));
    let bootstrap_hello = encode(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 1,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
    assert!(matches!(check_overlay_first(&bootstrap_hello), OverlayFirst::Bad(_)));
    assert!(matches!(check_overlay_first(b"not a frame at all"), OverlayFirst::Bad(_)));
}
