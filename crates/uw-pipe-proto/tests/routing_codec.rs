use uw_pipe_proto::{
    input::{InputReason, InputRecord},
    pipe::{self, OverlayMessage},
    routing::*,
};

fn maximum_config() -> RoutingConfig {
    let widgets = (1..=32)
        .map(|id| WidgetSpec {
            instance: WidgetRef { id, generation: 1 },
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            z: id as i32,
            mode: InputMode::Block,
            visible: true,
            keyboard: true,
        })
        .collect();
    let hotkeys = (32..=255)
        .filter(|vk| valid_trigger(*vk))
        .take(64)
        .enumerate()
        .map(|(n, vk)| HotkeySpec {
            id: n as u32 + 1,
            vk,
            modifiers: 0,
            flags: 0,
            action: if n == 0 { HotkeyAction::ToggleEdit } else { HotkeyAction::Notify },
            target: None,
        })
        .collect();
    RoutingConfig { epoch: 1, revision: 1, request_id: 1, width: 800, height: 600, widgets, hotkeys }
}
fn framed(message: RoutingMessage) -> Vec<u8> {
    pipe::encode_input(&OverlayMessage::Routing(message)).unwrap()
}

#[test]
fn maximum_configuration_fits_one_existing_frame() {
    let m = OverlayMessage::Routing(RoutingMessage::Configure(maximum_config()));
    let bytes = pipe::encode_input(&m).unwrap();
    assert_eq!(bytes.len(), pipe::HEADER_LEN + 2592);
    assert_eq!(pipe::decode_overlay(&bytes).unwrap(), m);
}

#[test]
fn maximum_routed_batch_fits_without_changing_input_record_shape() {
    let event = RoutedEvent {
        revision: 1,
        target: Some(WidgetRef { id: 1, generation: 1 }),
        kind: EventKind::Input,
        argument: 0,
        input: Some(InputRecord::text('😀', 0)),
    };
    let m = OverlayMessage::Routing(RoutingMessage::Events(RoutedBatch {
        epoch: 1,
        first_seq: 1,
        events: vec![event; 64],
    }));
    let bytes = pipe::encode_input(&m).unwrap();
    assert_eq!(bytes.len(), pipe::HEADER_LEN + 3092);
    assert_eq!(pipe::decode_overlay(&bytes).unwrap(), m);
}

#[test]
fn configuration_count_is_checked_before_allocating_tables() {
    let mut bytes = framed(RoutingMessage::Configure(maximum_config()));
    bytes[pipe::HEADER_LEN + 28..pipe::HEADER_LEN + 30].copy_from_slice(&33u16.to_le_bytes());
    assert!(pipe::decode_overlay(&bytes).is_err());
}

#[test]
fn reserved_record_padding_cannot_smuggle_another_payload() {
    let event = RoutedEvent {
        revision: 1,
        target: Some(WidgetRef { id: 1, generation: 1 }),
        kind: EventKind::Input,
        argument: 0,
        input: Some(InputRecord::motion(-1, 2, 0)),
    };
    let mut bytes = framed(RoutingMessage::Events(RoutedBatch { epoch: 1, first_seq: 1, events: vec![event] }));
    bytes[pipe::HEADER_LEN + 20 + 17] = 1;
    assert!(pipe::decode_overlay(&bytes).is_err());
}

#[test]
fn a_batch_cannot_wrap_the_epoch_sequence() {
    let event = RoutedEvent { revision: 1, target: None, kind: EventKind::Hotkey, argument: 1, input: None };
    let m = RoutingMessage::Events(RoutedBatch { epoch: 1, first_seq: u64::MAX, events: vec![event; 2] });
    assert!(pipe::encode_input(&OverlayMessage::Routing(m)).is_err());
}

#[test]
fn all_control_messages_roundtrip_with_exact_sizes() {
    let result = RoutingResult { epoch: 1, revision: 2, request_id: 3, code: RoutingCode::Applied, through_seq: 4 };
    let cases = vec![
        (RoutingMessage::Hello { module_version: 3, state_layout: 1, input_version: 2 }, 8),
        (RoutingMessage::Welcome { input_version: 2 }, 4),
        (RoutingMessage::Configured(result), 32),
        (RoutingMessage::Result(result), 32),
        (RoutingMessage::Visibility { state_seq: 1, on: false }, 16),
        (
            RoutingMessage::State(RoutingState {
                epoch: 1,
                revision: 1,
                state_seq: None,
                width: 800,
                height: 600,
                pointer: None,
                focus: None,
                takeover: None,
                exclusive: false,
                domains: Domains::default(),
                reason: InputReason::Ready,
            }),
            60,
        ),
        (
            RoutingMessage::Request(RoutingRequest {
                epoch: 1,
                revision: 2,
                request_id: 1,
                target: None,
                action: RoutingAction::EnterEdit,
                state_seq: None,
            }),
            40,
        ),
    ];
    for (m, size) in cases {
        let original = OverlayMessage::Routing(m);
        let bytes = pipe::encode_input(&original).unwrap();
        assert_eq!(bytes.len(), pipe::HEADER_LEN + size);
        assert_eq!(pipe::decode_overlay(&bytes).unwrap(), original);
    }
}

#[test]
fn every_truncated_configuration_and_appended_frame_is_rejected() {
    let bytes = framed(RoutingMessage::Configure(maximum_config()));
    for n in 0..bytes.len() {
        assert!(pipe::decode_overlay(&bytes[..n]).is_err(), "truncation {n}");
    }
    let mut appended = bytes;
    appended.push(0);
    assert!(pipe::decode_overlay(&appended).is_err());
}

#[test]
fn table_and_batch_growth_is_rejected_before_framing() {
    let mut c = maximum_config();
    let mut widget = c.widgets[0];
    widget.instance.id = 33;
    widget.z = 33;
    c.widgets.push(widget);
    assert!(pipe::encode_input(&OverlayMessage::Routing(RoutingMessage::Configure(c))).is_err());
    let mut c = maximum_config();
    c.hotkeys.push(HotkeySpec { id: 65, vk: 200, modifiers: 0, flags: 0, action: HotkeyAction::Notify, target: None });
    assert!(pipe::encode_input(&OverlayMessage::Routing(RoutingMessage::Configure(c))).is_err());
    let event = RoutedEvent { revision: 1, target: None, kind: EventKind::Hotkey, argument: 1, input: None };
    assert!(
        pipe::encode_input(&OverlayMessage::Routing(RoutingMessage::Events(RoutedBatch {
            epoch: 1,
            first_seq: 1,
            events: vec![event; 65]
        })))
        .is_err()
    );
}

#[test]
fn hello_has_independently_checked_little_endian_payload() {
    let bytes = framed(RoutingMessage::Hello { module_version: 0x01020304, state_layout: 1, input_version: 2 });
    assert_eq!(&bytes[4..6], &[0x20, 0x01]);
    assert_eq!(&bytes[pipe::HEADER_LEN..], &[4, 3, 2, 1, 1, 0, 2, 0]);
}

#[test]
fn unicode_surrogates_and_noninput_payloads_are_rejected() {
    let mut event = RoutedEvent {
        revision: 1,
        target: Some(WidgetRef { id: 1, generation: 1 }),
        kind: EventKind::Input,
        argument: 0,
        input: Some(InputRecord { kind: 2, flags: 0, a: 0xd800, b: 0, c: 0, d: 0, modifiers: 0 }),
    };
    assert!(event.validate().is_err());
    event.kind = EventKind::Cancel;
    event.argument = 1;
    assert!(event.validate().is_err());
    event.input = None;
    assert!(event.validate().is_ok());
}

#[test]
fn unsupported_native_api_cannot_advertise_an_unknown_abi() {
    let api = ApiCoverage {
        status: NativeStatus::Supported,
        domains: Domains { keyboard: true, mouse: true },
        abi_mask: 3,
        detail: 0,
    };
    let mut coverage = NativeCoverage {
        epoch: 1,
        revision: 1,
        directinput: api,
        gameinput: ApiCoverage {
            status: NativeStatus::Unsupported,
            domains: Domains::default(),
            abi_mask: 0,
            detail: 0x80004001,
        },
    };
    let m = OverlayMessage::Routing(RoutingMessage::Coverage(coverage));
    let bytes = pipe::encode_input(&m).unwrap();
    assert_eq!(bytes.len(), pipe::HEADER_LEN + 32);
    assert_eq!(pipe::decode_overlay(&bytes).unwrap(), m);
    coverage.gameinput.abi_mask = 16;
    assert!(RoutingMessage::Coverage(coverage).validate().is_err());
}

#[test]
fn unsupported_runtime_cannot_advertise_isolated_domains() {
    let unavailable = ApiCoverage {
        status: NativeStatus::Unsupported,
        domains: Domains { keyboard: true, mouse: false },
        abi_mask: 1,
        detail: 0x80004001,
    };
    let message = RoutingMessage::Coverage(NativeCoverage {
        epoch: 1,
        revision: 1,
        directinput: unavailable,
        gameinput: unavailable,
    });
    assert!(message.validate().is_err());
}
