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
