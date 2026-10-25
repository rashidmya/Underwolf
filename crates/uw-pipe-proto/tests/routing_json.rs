use uw_pipe_proto::{
    input::InputChallenge,
    pipe::{self, OverlayFirst, OverlayMessage},
    routing::{json::*, *},
};

#[test]
fn counters_above_javascript_precision_roundtrip_without_loss() {
    let original = RoutingEvent::RoutingConfigured {
        result: RoutingResult {
            epoch: 9_007_199_254_740_993,
            revision: u64::MAX,
            request_id: 1,
            code: RoutingCode::Applied,
            through_seq: u64::MAX,
        },
    };
    let line = original.to_line().unwrap();
    assert!(line.contains("\"9007199254740993\""));
    assert!(line.contains("\"18446744073709551615\""));
    assert_eq!(RoutingEvent::parse(&line).unwrap(), original);
}

#[test]
fn ack_counters_are_strings_and_numeric_json_is_rejected() {
    let command = RoutingCommand::RoutingAck {
        request_id: 1,
        ack: RoutingProgress::from(InputChallenge { epoch: 9_007_199_254_740_993, nonce: 2, through_seq: 0 }),
    };
    let line = command.to_line().unwrap();
    assert_eq!(RoutingCommand::parse(&line).unwrap(), command);
    assert!(RoutingCommand::parse(&line.replace("\"9007199254740993\"", "9007199254740993")).is_err());
}

#[test]
fn malformed_or_out_of_range_counter_strings_are_rejected() {
    for value in ["", "00", "01", "+1", "-1", " 1", "1 ", "18446744073709551616"] {
        assert!(parse_counter(value).is_err(), "{value:?}");
    }
    assert_eq!(parse_counter("0").unwrap(), 0);
}

#[test]
fn mismatched_nested_request_id_is_not_relayed() {
    let command = RoutingCommand::RouteRequest {
        request_id: 1,
        request: RoutingRequest {
            epoch: 1,
            revision: 1,
            request_id: 2,
            target: None,
            action: RoutingAction::EnterEdit,
            state_seq: None,
        },
    };
    assert!(command.to_line().is_err());
    let line = serde_json::to_string(&command).unwrap();
    assert!(RoutingCommand::parse(&line).is_err());
}

#[test]
fn v2_hello_rejects_v1_input_and_preserves_state_only_handshake() {
    let legacy = OverlayMessage::Input(uw_pipe_proto::input::InputMessage::Hello {
        module_version: 2,
        state_layout: 1,
        input_version: 1,
    });
    assert_eq!(classify_hello(&legacy), RoutingFirst::VersionMismatch);
    let current =
        OverlayMessage::Routing(RoutingMessage::Hello { module_version: 3, state_layout: 1, input_version: 2 });
    assert_eq!(classify_hello(&current), RoutingFirst::Hello { module_version: 3 });
    let wrong = OverlayMessage::Routing(RoutingMessage::Hello { module_version: 3, state_layout: 1, input_version: 1 });
    assert_eq!(classify_hello(&wrong), RoutingFirst::VersionMismatch);
    let state = OverlayMessage::Hello { module_version: 1, state_layout: 1 };
    assert_eq!(pipe::check_overlay_first(&pipe::encode_overlay(&state)), OverlayFirst::Hello { module_version: 1 });
}

#[test]
fn unknown_fields_and_oversized_json_cannot_be_accepted() {
    assert!(
        RoutingCommand::parse(
            r#"{"cmd":"routing_ack","request_id":1,"ack":{"epoch":"1","nonce":"1","through_seq":"0"},"extra":true}"#
        )
        .is_err()
    );
    assert!(RoutingCommand::parse(&" ".repeat(32_769)).is_err());
}
