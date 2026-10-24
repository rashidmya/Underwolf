use uw_input::policy::*;
use uw_pipe_proto::input::*;
fn ready() -> Policy {
    let mut p = Policy::new();
    for e in [
        PolicyEvent::Connected,
        PolicyEvent::Foreground(true),
        PolicyEvent::Visible(true),
        PolicyEvent::Healthy(true),
        PolicyEvent::Installed(true),
    ] {
        p.event(e);
    }
    p
}
fn key(p: &mut Policy, vk: i32, down: bool) -> Effects {
    p.event(PolicyEvent::Input(InputRecord::key(vk, 0, down, false, false, 0)))
}
fn request(p: &mut Policy, on: bool) -> Effects {
    p.event(PolicyEvent::Request(CaptureRequest { epoch: p.epoch(), state_seq: 1, request_id: 1, on }))
}
fn enter(p: &mut Policy) {
    assert!(matches!(request(p, true).transition, Some(Transition::Enter { .. })));
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(p.active());
}
#[test]
fn chord_has_one_edge_and_tracks_both_ctrl_keys() {
    let mut p = ready();
    key(&mut p, 0xa2, true);
    key(&mut p, 0xa3, true);
    key(&mut p, 0xa2, false);
    assert!(matches!(key(&mut p, 9, true).transition, Some(Transition::Enter { .. })));
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    for _ in 0..5 {
        assert!(key(&mut p, 9, true).transition.is_none());
        assert!(p.active());
    }
    key(&mut p, 9, false);
    assert!(matches!(key(&mut p, 9, true).transition, Some(Transition::Release { .. })));
    assert!(!p.active());
}
#[test]
fn held_until_up_never_represses() {
    let mut p = ready();
    assert!(key(&mut p, 87, true).pass);
    p.event(PolicyEvent::Input(InputRecord::button(0, true, 0, 0, 0)));
    let e = request(&mut p, true);
    assert_eq!(e.releases.iter().flatten().count(), 2);
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(!key(&mut p, 65, true).pass);
    assert_eq!(key(&mut p, 65, false).host.iter().flatten().count(), 1);
    request(&mut p, false);
    p.event(PolicyEvent::Restored { epoch: p.epoch() });
    assert!(!key(&mut p, 87, true).pass);
    assert!(!key(&mut p, 87, false).pass);
    assert!(key(&mut p, 87, true).pass);
}
#[test]
fn loss_invalidates_queued_enter() {
    for loss in [
        PolicyEvent::Foreground(false),
        PolicyEvent::Visible(false),
        PolicyEvent::Disconnected,
        PolicyEvent::Healthy(false),
        PolicyEvent::Failure(InputReason::Overflow),
        PolicyEvent::Destroyed,
    ] {
        let mut p = ready();
        enter(&mut p);
        let epoch = p.epoch();
        let old = CaptureRequest { epoch, state_seq: 5, request_id: 4, on: true };
        let release = p.event(loss);
        assert!(!p.active());
        assert!(p.epoch() > epoch);
        assert!(matches!(release.transition, Some(Transition::Release { .. })));
        let new_epoch = p.epoch();
        assert!(p.event(loss).transition.is_none());
        assert_eq!(p.epoch(), new_epoch);
        assert_eq!(p.event(PolicyEvent::Request(old)).result.unwrap().code, CaptureCode::StaleEpoch);
        p.event(PolicyEvent::Foreground(true));
        p.event(PolicyEvent::Visible(true));
        p.event(PolicyEvent::Healthy(true));
        assert!(!p.active());
    }
}
#[test]
fn setup_failure_rolls_back_without_reporting_applied() {
    let mut p = ready();
    request(&mut p, true);
    let epoch = p.epoch();
    let failed = p.event(PolicyEvent::SetupFinished { epoch, success: false });
    assert!(!p.active());
    assert_eq!(failed.result.unwrap().code, CaptureCode::SetupFailed);
    assert!(matches!(failed.transition, Some(Transition::Release { .. })));
    assert!(p.event(PolicyEvent::SetupFinished { epoch, success: true }).result.is_none());
    assert!(!p.active());
}
#[test]
fn loss_while_entering_invalidates_late_completion() {
    let mut p = ready();
    request(&mut p, true);
    let old = p.epoch();
    p.event(PolicyEvent::Foreground(false));
    p.event(PolicyEvent::SetupFinished { epoch: old, success: true });
    assert!(!p.active());
}
#[test]
fn text_is_emitted_only_from_char_messages() {
    let mut p = ready();
    enter(&mut p);
    let keys = key(&mut p, 65, true);
    assert_eq!(keys.host.iter().flatten().filter(|r| r.kind == 2).count(), 0);
    assert!(p.event(PolicyEvent::Char(0xd83d)).host.iter().all(Option::is_none));
    assert_eq!(p.event(PolicyEvent::Char(0xde00)).host[0], Some(InputRecord::text('😀', 0)));
}
#[test]
fn no_capture_without_all_preconditions() {
    let mut p = Policy::new();
    assert_eq!(request(&mut p, true).result.unwrap().code, CaptureCode::Unavailable);
    assert!(key(&mut p, 65, true).pass);
}
#[test]
fn exit_does_not_acknowledge_before_restoration() {
    let mut p = ready();
    enter(&mut p);
    let e = request(&mut p, false);
    assert!(e.result.is_none());
    assert!(!p.active());
    let done = p.event(PolicyEvent::Restored { epoch: p.epoch() });
    assert_eq!(done.result.unwrap().code, CaptureCode::Applied);
}

#[test]
fn focus_loss_invalidates_requests_even_before_capture_starts() {
    let mut p = ready();
    let epoch = p.epoch();
    p.event(PolicyEvent::Foreground(false));
    p.event(PolicyEvent::Foreground(true));
    assert_eq!(
        p.event(PolicyEvent::Request(CaptureRequest { epoch, state_seq: 9, request_id: 3, on: true }))
            .result
            .unwrap()
            .code,
        CaptureCode::StaleEpoch
    );
}
