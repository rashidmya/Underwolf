use uw_input::policy::*;
use uw_pipe_proto::{input::*, routing::Domains};
fn ready() -> Policy {
    let mut p = Policy::new();
    for event in [
        PolicyEvent::Connected,
        PolicyEvent::Foreground(true),
        PolicyEvent::Visible(true),
        PolicyEvent::Healthy(true),
        PolicyEvent::Installed(true),
    ] {
        p.event(event);
    }
    p
}
fn key(vk: i32, down: bool) -> InputRecord {
    InputRecord::key(vk, 0, down, false, false, 0)
}
fn enter(p: &mut Policy) {
    p.event(PolicyEvent::Request(CaptureRequest { epoch: p.epoch(), state_seq: 1, request_id: 1, on: true }));
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(p.active());
}

#[test]
fn configured_local_ownership_changes_keep_the_delivery_session_healthy() {
    let mut p = ready();
    p.use_configured_hotkeys();
    let epoch = p.epoch();
    let effects = p.own_domains(Domains { keyboard: false, mouse: true }, true);
    assert!(matches!(effects.transition, Some(Transition::Enter { .. })));
    p.event(PolicyEvent::SetupFinished { epoch, success: true });
    assert!(p.active());
    let released = p.own_domains(Domains::default(), true);
    assert_eq!(p.epoch(), epoch);
    assert!(matches!(released.transition, Some(Transition::Release { .. })));
    p.event(PolicyEvent::Restored { epoch });
    assert!(matches!(
        p.own_domains(Domains { keyboard: true, mouse: false }, true).transition,
        Some(Transition::Enter { .. })
    ));
}

#[test]
fn domain_application_rechecks_the_current_lease_before_retaining_ownership() {
    let mut p = ready();
    p.use_configured_hotkeys();
    p.own_domains(Domains { keyboard: true, mouse: false }, true);
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(p.active());
    let effects = p.own_domains(Domains { keyboard: true, mouse: false }, false);
    assert!(!p.active());
    assert!(matches!(effects.transition, Some(Transition::Release { reason: InputReason::LeaseExpired, .. })));
}

#[test]
fn mouse_ownership_does_not_release_or_filter_game_keyboard() {
    let mut p = ready();
    p.event(PolicyEvent::Input(key(87, true)));
    p.set_domains(Domains { keyboard: false, mouse: true });
    let request =
        p.event(PolicyEvent::Request(CaptureRequest { epoch: p.epoch(), state_seq: 1, request_id: 1, on: true }));
    assert_eq!(request.releases.iter().flatten().count(), 0);
    p.event(PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(p.event(PolicyEvent::Input(key(65, true))).pass);
    assert!(p.event(PolicyEvent::Input(InputRecord::motion(10, 20, 0))).host[0].is_some());
}

#[test]
fn partial_domain_transfer_preserves_epoch_and_quarantines_held_keys() {
    let mut p = ready();
    enter(&mut p);
    p.event(PolicyEvent::Input(key(87, true)));
    let epoch = p.epoch();
    p.set_domains(Domains { keyboard: false, mouse: true });
    assert_eq!(p.epoch(), epoch);
    assert!(p.active());
    assert!(!p.event(PolicyEvent::Input(key(87, true))).pass);
    assert!(!p.event(PolicyEvent::Input(key(87, false))).pass);
    assert!(p.event(PolicyEvent::Input(key(87, true))).pass);
}

#[test]
fn gaining_keyboard_releases_only_game_seen_keys() {
    let mut p = ready();
    p.set_domains(Domains { keyboard: false, mouse: true });
    enter(&mut p);
    p.event(PolicyEvent::Input(key(87, true)));
    let effects = p.set_domains(Domains { keyboard: true, mouse: true });
    let releases: Vec<_> = effects.releases.iter().flatten().collect();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].a, 87);
    assert_eq!(releases[0].flags & 1, 0);
}

#[test]
fn consumed_passthrough_down_does_not_create_a_phantom_game_release() {
    let mut p = ready();
    p.event(PolicyEvent::RoutedInput { record: key(65, true), pass: false });
    let effects =
        p.event(PolicyEvent::Request(CaptureRequest { epoch: p.epoch(), state_seq: 1, request_id: 1, on: true }));
    assert_eq!(effects.releases.iter().flatten().count(), 0);
}

#[test]
fn configured_hotkeys_disable_the_legacy_ctrl_tab_toggle() {
    let mut p = ready();
    p.use_configured_hotkeys();
    p.event(PolicyEvent::Input(key(162, true)));
    let effects = p.event(PolicyEvent::Input(key(9, true)));
    assert!(effects.transition.is_none());
    assert!(effects.pass);
    assert!(!p.active());
}

#[test]
fn passive_text_notification_does_not_take_keyboard_input() {
    let mut p = ready();
    p.text_notifications(true);
    let effects = p.event(PolicyEvent::Char('x' as u16));
    assert!(effects.pass);
    assert_eq!(effects.host[0].unwrap().a, 'x' as i32);
    p.text_notifications(false);
    assert!(p.event(PolicyEvent::Char('y' as u16)).host[0].is_none());
}

#[test]
fn no_input_domains_cannot_be_reported_as_active_capture() {
    let mut p = ready();
    p.set_domains(Domains::default());
    let result =
        p.event(PolicyEvent::Request(CaptureRequest { epoch: p.epoch(), state_seq: 1, request_id: 1, on: true }));
    assert_eq!(result.result.unwrap().code, CaptureCode::Unavailable);
}

#[test]
fn scalar_text_notification_has_the_same_passthrough_contract() {
    let mut p = ready();
    p.text_notifications(true);
    let result = p.event(PolicyEvent::Input(InputRecord::text('😀', 0)));
    assert!(result.pass);
    assert_eq!(result.host[0], Some(InputRecord::text('😀', 0)));
}

#[test]
fn consumed_routed_key_is_hidden_from_polling_until_its_release() {
    let mut p = ready();
    p.use_configured_hotkeys();
    assert!(!p.event(PolicyEvent::RoutedInput { record: key(9, true), pass: false }).pass);
    assert!(p.key_suppressed(9));
    assert!(!p.event(PolicyEvent::RoutedInput { record: key(9, false), pass: false }).pass);
    assert!(!p.key_suppressed(9));
    assert!(p.event(PolicyEvent::Input(key(9, true))).pass);
}
