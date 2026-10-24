use uw_pipe_proto::input::*;
use uw_pipe_proto::jsonl::{Command, Event};
use uw_pipe_proto::pipe::{self, OverlayFirst, OverlayMessage, ProtoError};

fn messages() -> Vec<InputMessage> {
    vec![
        InputMessage::Hello { module_version: 2, state_layout: 1, input_version: 1 },
        InputMessage::Welcome { input_version: 1 },
        InputMessage::State(InputState {
            epoch: 1,
            active: false,
            reason: InputReason::Ready,
            state_seq: None,
            width: 800,
            height: 600,
        }),
        InputMessage::Batch(InputBatch {
            epoch: 1,
            first_seq: 1,
            records: vec![
                InputRecord::key(65, 30, true, false, false, 0),
                InputRecord::text('😀', 0),
                InputRecord::motion(-5, 8, 0),
                InputRecord::button(4, true, -5, 8, 0),
                InputRecord::wheel(120, -120, -5, 8, 0),
            ],
        }),
        InputMessage::Challenge(InputChallenge { epoch: 1, nonce: 3, through_seq: 5 }),
        InputMessage::Ack(InputAck { epoch: 1, nonce: 3, through_seq: 5 }),
        InputMessage::Capture(CaptureRequest { epoch: 1, state_seq: 9, request_id: 7, on: true }),
        InputMessage::Result(CaptureResult { epoch: 1, state_seq: 9, request_id: 7, code: CaptureCode::Applied }),
    ]
}
fn wire(m: InputMessage) -> Vec<u8> {
    pipe::encode_input(&OverlayMessage::Input(m)).unwrap()
}

#[test]
fn input_wire_roundtrip_and_limits() {
    for (i, m) in messages().into_iter().enumerate() {
        let bytes = wire(m.clone());
        assert_eq!(&bytes[..4], b"UWP1");
        assert_eq!(u16::from_le_bytes(bytes[4..6].try_into().unwrap()), 0x110 + i as u16);
        assert_eq!(pipe::decode_overlay(&bytes).unwrap(), OverlayMessage::Input(m));
        for end in 0..bytes.len() {
            assert!(pipe::decode_overlay(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        let len = (trailing.len() - 16) as u32;
        trailing[12..16].copy_from_slice(&len.to_le_bytes());
        assert!(pipe::decode_overlay(&trailing).is_err());
    }
    let mut batch = InputBatch { epoch: 1, first_seq: 1, records: vec![InputRecord::motion(1, 2, 0); 64] };
    assert_eq!(wire(InputMessage::Batch(batch.clone())).len(), 16 + 1556);
    batch.records.push(InputRecord::motion(3, 4, 0));
    assert_eq!(pipe::encode_input(&OverlayMessage::Input(InputMessage::Batch(batch))), Err(ProtoError::BadInput));
    assert_eq!(wire(messages()[2].clone()).len(), 44);
    assert_eq!(wire(messages()[6].clone()).len(), 40);
    assert_eq!(wire(messages()[7].clone()).len(), 40);
}

#[test]
fn malformed_input_is_rejected() {
    // Deliberate mutations of independent byte offsets, not a mirrored encoder.
    for (index, offset, value) in [
        (1, 18, 1),
        (2, 16, 0),
        (2, 24, 2),
        (2, 25, 255),
        (2, 26, 2),
        (2, 27, 1),
        (3, 34, 1),
        (3, 36, 0),
        (3, 37, 128),
        (3, 38, 1),
        (3, 56, 16),
        (6, 36, 2),
        (6, 37, 1),
        (7, 36, 255),
        (7, 38, 1),
    ] {
        let mut bytes = wire(messages()[index].clone());
        bytes[offset] = value;
        assert!(pipe::decode_overlay(&bytes).is_err(), "index={index} offset={offset}");
    }
    for record in [
        InputRecord::button(5, true, 0, 0, 0),
        InputRecord::key(0, 0, true, false, false, 0),
        InputRecord { kind: 2, flags: 0, a: 0xd800, b: 0, c: 0, d: 0, modifiers: 0 },
    ] {
        assert!(
            pipe::encode_input(&OverlayMessage::Input(InputMessage::Batch(InputBatch {
                epoch: 1,
                first_seq: 1,
                records: vec![record]
            })))
            .is_err()
        );
    }
    for (epoch, seq, count) in [(0, 1, 1), (1, 0, 1), (1, 1, 0), (1, u64::MAX, 2)] {
        assert!(
            pipe::encode_input(&OverlayMessage::Input(InputMessage::Batch(InputBatch {
                epoch,
                first_seq: seq,
                records: vec![InputRecord::motion(1, 2, 0); count]
            })))
            .is_err()
        );
    }
}

#[test]
fn input_hello_is_distinct_from_legacy() {
    let legacy = pipe::encode_overlay(&OverlayMessage::Hello { module_version: 1, state_layout: 1 });
    assert_eq!(&legacy[4..6], &[1, 1]);
    assert_eq!(pipe::check_overlay_first(&legacy), OverlayFirst::Hello { module_version: 1 });
    assert_eq!(pipe::check_overlay_first(&wire(messages()[0].clone())), OverlayFirst::InputHello { module_version: 2 });
    assert_eq!(
        pipe::check_overlay_first(&wire(InputMessage::Hello { module_version: 2, state_layout: 1, input_version: 2 })),
        OverlayFirst::VersionMismatch
    );
}

#[test]
fn input_json_request_ids() {
    let ack = InputAck { epoch: 1, nonce: 1, through_seq: 0 };
    let cmd = Command::InputAck { request_id: 7, ack };
    assert_eq!(cmd.request_id(), 7);
    assert_eq!(Command::parse(&cmd.to_line()).unwrap(), cmd);
    assert_eq!(Event::InputAcked { request_id: 7 }.request_id(), Some(7));
    let result = Event::CaptureResult { epoch: 1, state_seq: 9, capture_request_id: 7, code: CaptureCode::Applied };
    assert_eq!(result.request_id(), None);
    assert_eq!(Event::parse(&result.to_line()).unwrap(), result);
    let old = Command::parse(r#"{"cmd":"set_state","request_id":1,"edit_mode":true}"#).unwrap();
    assert!(matches!(old, Command::SetState { input_epoch: None, .. }));
    let modern = Command::parse(r#"{"cmd":"set_state","request_id":2,"edit_mode":true,"input_epoch":4}"#).unwrap();
    assert!(matches!(modern, Command::SetState { input_epoch: Some(4), .. }));
}
