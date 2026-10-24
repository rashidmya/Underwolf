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

#[test]
fn polling_aliases_and_mouse_follow_held_quarantine() {
    let mut p = ready();
    key(&mut p, 162, true);
    p.event(PolicyEvent::Input(InputRecord::button(1, true, 0, 0, 0)));
    enter(&mut p);
    request(&mut p, false);
    p.event(PolicyEvent::Restored { epoch: p.epoch() });
    assert!(p.key_suppressed(17));
    assert!(p.key_suppressed(162));
    assert!(p.key_suppressed(4));
    key(&mut p, 162, false);
    p.event(PolicyEvent::Input(InputRecord::button(1, false, 0, 0, 0)));
    assert!(!p.key_suppressed(17));
    assert!(!p.key_suppressed(4));
}

#[test]
fn raw_observation_tracks_game_held_keys_and_never_duplicates_captured_input() {
    let mut p = ready();
    p.event(PolicyEvent::Observed(InputRecord::key(87, 17, true, false, false, 0)));
    let entry = request(&mut p, true);
    assert!(entry.releases.iter().flatten().any(|r| r.a == 87));
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    let observed = p.event(PolicyEvent::Observed(InputRecord::key(65, 30, true, false, false, 0)));
    assert!(observed.host.iter().all(Option::is_none));
    assert_eq!(key(&mut p, 65, true).host.iter().flatten().count(), 1);
}
#[test]
fn hiding_state_is_applied_only_after_restoration() {
    let mut p = ready();
    enter(&mut p);
    let effects = p.event(PolicyEvent::VisibleState { on: false, state_seq: 9 });
    assert!(effects.applied.is_none());
    assert!(!p.active());
    assert_eq!(p.event(PolicyEvent::Restored { epoch: p.epoch() }).applied, Some(9));
    assert_eq!(p.event(PolicyEvent::VisibleState { on: true, state_seq: 10 }).applied, Some(10));
    assert!(!p.active());
}

#[test]
fn reconnect_while_restoring_reissues_restore_for_the_new_epoch() {
    let mut p = ready();
    enter(&mut p);
    request(&mut p, false);
    let old = p.epoch();
    let lost = p.event(PolicyEvent::Disconnected);
    assert!(p.epoch() > old);
    assert!(matches!(lost.transition,Some(Transition::Release{epoch,..}) if epoch==p.epoch()));
    assert_eq!(lost.result.unwrap().code, CaptureCode::Superseded);
    let connected = p.event(PolicyEvent::Connected);
    assert!(matches!(connected.transition,Some(Transition::Release{epoch,..}) if epoch==p.epoch()));
    p.event(PolicyEvent::Restored { epoch: p.epoch() });
    assert_eq!(p.mode(), Mode::Inactive);
}
#[test]
fn startup_modifier_aliases_do_not_leave_a_phantom_ctrl() {
    let mut p = ready();
    for vk in [17, 162] {
        p.event(PolicyEvent::Snapshot(InputRecord::key(vk, 29, true, false, false, 0)));
    }
    key(&mut p, 162, false);
    assert_eq!(p.modifiers() & 2, 0);
    assert!(key(&mut p, 9, true).transition.is_none());
}

#[test]
fn capture_entry_uses_current_lease_not_queued_health() {
    let mut p = ready();
    p.event_with_lease(PolicyEvent::Healthy(true), false);
    let r = CaptureRequest { epoch: p.epoch(), state_seq: 7, request_id: 9, on: true };
    let e = p.event_with_lease(PolicyEvent::Request(r), false);
    assert_eq!(e.result.unwrap().code, CaptureCode::Unavailable);
    assert!(e.transition.is_none());
    p.event_with_lease(PolicyEvent::Input(InputRecord::key(0xa2, 0, true, false, false, 0)), false);
    let e = p.event_with_lease(PolicyEvent::Input(InputRecord::key(9, 0, true, false, false, 0)), false);
    assert!(e.transition.is_none());
    assert!(!p.active());
}

#[test]
fn lease_expiring_during_setup_never_reports_applied() {
    let mut p = ready();
    request(&mut p, true);
    let e = p.event_with_lease(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true }, false);
    assert_eq!(e.result.unwrap().code, CaptureCode::SetupFailed);
    assert!(!p.active());
}

#[test]
fn hide_and_edit_off_apply_as_one_correlated_transaction() {
    let mut p = ready();
    enter(&mut p);
    let r = CaptureRequest { epoch: p.epoch(), state_seq: 42, request_id: 19, on: false };
    let e = p.event(PolicyEvent::CaptureState { request: r, visible: false, state_seq: 42 });
    assert!(e.result.is_none());
    assert!(matches!(e.transition, Some(Transition::Release { .. })));
    let e = p.event(PolicyEvent::Restored { epoch: p.epoch() });
    let result = e.result.unwrap();
    assert_eq!(result.code, CaptureCode::Applied);
    assert_eq!(result.state_seq, 42);
    assert_eq!(result.request_id, 19);
    assert_eq!(result.epoch, r.epoch);
}

#[test]
fn overwritten_capture_state_never_applies_later_visibility() {
    let mut p = ready();
    let r = CaptureRequest { epoch: p.epoch(), state_seq: 42, request_id: 19, on: true };
    let e = p.event(PolicyEvent::CaptureState { request: r, visible: false, state_seq: 43 });
    assert_eq!(e.result.unwrap().code, CaptureCode::Superseded);
    assert!(request(&mut p, true).transition.is_some());
}
