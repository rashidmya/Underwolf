use uw_helper::input::*;
use uw_pipe_proto::input::*;
#[test]
fn queued_old_epoch_command_is_refused() {
    let mut r = InputRelay::default();
    assert!(r.request_epoch(Some(1)).is_err());
    r.accept(InputMessage::State(InputState {
        epoch: 2,
        active: false,
        reason: InputReason::Ready,
        state_seq: None,
        width: 800,
        height: 600,
    }))
    .unwrap();
    assert!(r.request_epoch(None).is_err());
    assert!(r.request_epoch(Some(1)).is_err());
    assert_eq!(r.request_epoch(Some(2)), Ok(2));
}
#[test]
fn same_value_requests_have_distinct_results() {
    let mut r = InputRelay::default();
    for id in [1, 2] {
        let event = r
            .accept(InputMessage::Result(CaptureResult {
                epoch: 1,
                state_seq: 4,
                request_id: id,
                code: CaptureCode::Applied,
            }))
            .unwrap();
        assert!(
            matches!(event,uw_pipe_proto::jsonl::Event::CaptureResult{capture_request_id,..} if capture_request_id==id)
        );
    }
}
#[test]
fn application_requests_are_bounded_correlated_and_expire() {
    let mut p = uw_helper::input::CapturePending::default();
    for id in 0..32 {
        p.add(CaptureRequest { epoch: 1, state_seq: 7, request_id: id, on: true }, 100).unwrap();
    }
    assert!(p.add(CaptureRequest { epoch: 1, state_seq: 7, request_id: 33, on: true }, 100).is_err());
    assert!(!p.take(CaptureResult { epoch: 2, state_seq: 7, request_id: 0, code: CaptureCode::Applied }));
    assert!(p.take(CaptureResult { epoch: 1, state_seq: 7, request_id: 0, code: CaptureCode::Applied }));
    assert!(p.expire(5099).is_empty());
    assert_eq!(p.expire(5100).len(), 31);
    assert!(!p.take(CaptureResult { epoch: 1, state_seq: 7, request_id: 1, code: CaptureCode::Applied }));
}
