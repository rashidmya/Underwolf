use uw_input::{configuration::PreparedConfig, routing::*};
use uw_pipe_proto::{
    input::{InputReason, InputRecord},
    routing::*,
};
fn instance(id: u32) -> WidgetRef {
    WidgetRef { id, generation: 1 }
}
fn table(mode: InputMode) -> PreparedConfig {
    PreparedConfig::compile(RoutingConfig {
        epoch: 1,
        revision: 1,
        request_id: 1,
        width: 800,
        height: 600,
        widgets: vec![
            WidgetSpec {
                instance: instance(1),
                x: 10,
                y: 10,
                width: 100,
                height: 100,
                z: 1,
                mode,
                visible: true,
                keyboard: true,
            },
            WidgetSpec {
                instance: instance(2),
                x: 50,
                y: 10,
                width: 100,
                height: 100,
                z: 2,
                mode,
                visible: true,
                keyboard: true,
            },
        ],
        hotkeys: vec![HotkeySpec {
            id: 1,
            vk: 9,
            modifiers: 8,
            flags: 0,
            action: HotkeyAction::ToggleEdit,
            target: None,
        }],
    })
    .unwrap()
}
fn context() -> RouteContext {
    RouteContext {
        epoch: 1,
        foreground: true,
        visible: true,
        healthy: true,
        cursor_available: true,
        physical_keys: [false; 256],
        physical_buttons: [false; 5],
    }
}
fn request(action: RoutingAction, target: Option<WidgetRef>) -> RoutingRequest {
    RoutingRequest { epoch: 1, revision: 1, request_id: 1, target, action, state_seq: None }
}
fn input_count(effects: &RouteEffects) -> usize {
    effects.events.iter().flatten().filter(|e| e.kind == EventKind::Input).count()
}

#[test]
fn synchronized_safety_epoch_keeps_table_but_requires_a_fresh_click() {
    let mut r = Router::new();
    r.configure(&table(InputMode::Block));
    r.input(InputRecord::button(0, true, 20, 20, 0), context());
    r.revoke(InputReason::FocusLost);
    r.advance_epoch(2);
    let ctx = RouteContext { epoch: 2, ..context() };
    assert_eq!(r.input(InputRecord::motion(20, 20, 0), ctx).domains, Domains::default());
    r.input(InputRecord::button(0, false, 20, 20, 0), ctx);
    assert!(r.input(InputRecord::button(0, true, 20, 20, 0), ctx).domains.mouse);
}

#[test]
fn a_background_release_cannot_leave_a_cancelled_button_latched_forever() {
    let mut r = Router::new();
    r.configure(&table(InputMode::Block));
    r.input(InputRecord::button(0, true, 20, 20, 0), context());
    r.revoke(InputReason::FocusLost);
    r.physical_release(InputRecord::button(0, false, 20, 20, 0));
    assert!(r.input(InputRecord::button(0, true, 20, 20, 0), context()).domains.mouse);
}
fn body(router: &mut Router, button: usize, down: bool, x: i32, y: i32) -> RouteEffects {
    let mut c = context();
    c.physical_buttons[button] = down;
    router.input(InputRecord::button(button as i32, down, x, y, 0), c)
}

#[test]
fn standard_notification_is_passive_and_targets_the_top_widget() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Notify));
    let e = router.input(InputRecord::motion(60, 20, 0), context());
    assert!(e.pass);
    assert_eq!(e.domains, Domains::default());
    assert_eq!(input_count(&e), 1);
    let event = e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap();
    assert_eq!(event.target, Some(instance(2)));
    assert_eq!(event.input.unwrap().a, 10);
}

#[test]
fn block_mode_arms_on_a_fresh_click_and_preserves_pointer_capture() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    assert_eq!(router.input(InputRecord::motion(60, 20, 0), context()).domains, Domains::default());
    let down = body(&mut router, 0, true, 60, 20);
    assert!(!down.pass);
    assert_eq!(down.domains, Domains { keyboard: true, mouse: true });
    let moved = router.input(InputRecord::motion(400, 300, 0), context());
    assert_eq!(moved.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap().target, Some(instance(2)));
    assert_eq!(input_count(&body(&mut router, 0, false, 400, 300)), 1);
}

#[test]
fn game_click_clears_standard_keyboard_focus() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    body(&mut router, 0, false, 20, 20);
    let e = body(&mut router, 0, true, 500, 500);
    assert!(e.pass);
    assert_eq!(e.domains, Domains::default());
    assert!(
        e.events.iter().flatten().any(|event| event.kind == EventKind::Cancel && event.target == Some(instance(1)))
    );
}

#[test]
fn exclusive_background_exit_consumes_both_button_edges() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Pass));
    assert_eq!(router.request(request(RoutingAction::EnterEdit, None), context()).code, Some(RoutingCode::Applied));
    let down = body(&mut router, 0, true, 500, 500);
    assert!(!down.pass);
    assert!(!down.exclusive);
    assert!(!body(&mut router, 0, false, 500, 500).pass);
}

#[test]
fn exclusive_mode_makes_pass_windows_interactive() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Pass));
    router.request(request(RoutingAction::EnterEdit, None), context());
    let e = body(&mut router, 0, true, 60, 20);
    assert!(!e.pass);
    assert_eq!(input_count(&e), 1);
}

#[test]
fn stale_request_cannot_release_a_new_takeover_owner() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    router.request(request(RoutingAction::Takeover, Some(instance(1))), context());
    let mut stale = request(RoutingAction::ReleaseTakeover, Some(instance(1)));
    stale.epoch = 2;
    let e = router.request(stale, context());
    assert_eq!(e.code, Some(RoutingCode::StaleEpoch));
    assert!(e.exclusive);
    let e = router.request(request(RoutingAction::ReleaseTakeover, Some(instance(2))), context());
    assert_eq!(e.code, Some(RoutingCode::NotOwner));
    assert!(e.exclusive);
}

#[test]
fn reconnect_and_stationary_hover_do_not_recapture() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    router.revoke(InputReason::Disconnected);
    let mut c = context();
    c.epoch = 2;
    assert_eq!(router.input(InputRecord::motion(20, 20, 0), c).domains, Domains::default());
}

#[test]
fn moving_a_widget_does_not_discard_its_paired_release() {
    let original = table(InputMode::Block);
    let mut router = Router::new();
    router.configure(&original);
    body(&mut router, 0, true, 20, 20);
    let mut next = original.config().clone();
    next.revision = 2;
    next.widgets[0].x = 200;
    router.configure(&PreparedConfig::compile(next).unwrap());
    let e = body(&mut router, 0, false, 220, 20);
    let event = e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap();
    assert_eq!(event.target, Some(instance(1)));
    assert_eq!(event.revision, 2);
    assert_eq!(event.input.unwrap().b, 20);
}

#[test]
fn replacing_an_instance_cancels_old_input_without_sending_its_up_to_the_new_one() {
    let original = table(InputMode::Block);
    let mut router = Router::new();
    router.configure(&original);
    body(&mut router, 0, true, 20, 20);
    let mut next = original.config().clone();
    next.revision = 2;
    next.widgets[0].instance.generation = 2;
    let effects = router.configure(&PreparedConfig::compile(next).unwrap());
    assert!(effects.events.iter().flatten().any(|e| e.kind == EventKind::Cancel && e.target == Some(instance(1))));
    assert_eq!(input_count(&body(&mut router, 0, false, 20, 20)), 0);
}

#[test]
fn ending_a_drag_outside_releases_mouse_without_losing_keyboard_focus() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    let e = body(&mut router, 0, false, 400, 300);
    assert_eq!(e.domains, Domains { keyboard: true, mouse: false });
}

#[test]
fn hidden_game_cursor_cannot_preserve_standard_blocking_ownership() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    let mut c = context();
    c.cursor_available = false;
    let e = router.input(InputRecord::motion(20, 20, 0), c);
    assert_eq!(e.domains, Domains::default());
    assert!(e.pass);
}

#[test]
fn observing_an_expired_lease_in_a_request_releases_existing_capture() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    router.request(request(RoutingAction::EnterEdit, None), context());
    let mut c = context();
    c.healthy = false;
    let e = router.request(request(RoutingAction::EnterEdit, None), c);
    assert_eq!(e.code, Some(RoutingCode::Unavailable));
    assert_eq!(e.domains, Domains::default());
    assert!(!e.exclusive);
}

#[test]
fn a_stale_queued_input_does_not_release_a_new_capture() {
    let mut router = Router::new();
    let mut config = table(InputMode::Block).config().clone();
    config.epoch = 2;
    config.revision = 2;
    router.configure(&PreparedConfig::compile(config).unwrap());
    let mut c = context();
    c.epoch = 2;
    let mut r = request(RoutingAction::EnterEdit, None);
    r.epoch = 2;
    r.revision = 2;
    router.request(r, c);
    let e = router.input(InputRecord::motion(20, 20, 0), context());
    assert!(e.exclusive);
    assert_eq!(input_count(&e), 0);
}

#[test]
fn pass_window_does_not_obstruct_an_interactive_window_below_it() {
    let mut c = table(InputMode::Block).config().clone();
    c.widgets[1].mode = InputMode::Pass;
    let mut router = Router::new();
    router.configure(&PreparedConfig::compile(c).unwrap());
    let e = body(&mut router, 0, true, 60, 20);
    assert_eq!(e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap().target, Some(instance(1)));
}

#[test]
fn clipping_preserves_local_coordinates_and_excludes_the_right_edge() {
    let mut c = table(InputMode::Block).config().clone();
    c.widgets[0].x = -20;
    c.widgets[1].visible = false;
    let mut router = Router::new();
    router.configure(&PreparedConfig::compile(c).unwrap());
    let e = body(&mut router, 0, true, 0, 20);
    assert_eq!(e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap().input.unwrap().b, 20);
    body(&mut router, 0, false, 0, 20);
    assert!(body(&mut router, 0, true, 80, 20).pass);
}

#[test]
fn five_buttons_keep_capture_until_the_final_up_and_route_both_wheels() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    for button in 0..5 {
        assert_eq!(input_count(&body(&mut router, button, true, 20, 20)), 1);
    }
    let e = router.input(InputRecord::wheel(120, -120, 300, 300, 0), context());
    let record = e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap().input.unwrap();
    assert_eq!((record.a, record.b, record.c, record.d), (120, -120, 290, 290));
    assert!(!e.pass);
    for button in 0..4 {
        assert_eq!(input_count(&body(&mut router, button, false, 300, 300)), 1);
    }
    let e = body(&mut router, 4, false, 300, 300);
    assert_eq!(e.domains, Domains { keyboard: true, mouse: false });
}

#[test]
fn focus_transfer_cancels_held_keys_and_text_goes_to_the_new_target() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    body(&mut router, 0, false, 20, 20);
    let mut c = context();
    c.physical_keys[87] = true;
    let down = router.input(InputRecord::key(87, 0, true, false, false, 0), c);
    assert_eq!(input_count(&down), 1);
    let transfer = router.input(InputRecord::button(0, true, 120, 20, 0), c);
    assert!(transfer.events.iter().flatten().any(|e| e.kind == EventKind::Cancel && e.target == Some(instance(1))));
    assert_eq!(input_count(&router.input(InputRecord::key(87, 0, true, false, true, 0), c)), 0);
    c.physical_keys[87] = false;
    assert_eq!(input_count(&router.input(InputRecord::key(87, 0, false, false, false, 0), c)), 0);
    let e = router.input(InputRecord::text('😀', 0), c);
    assert_eq!(e.events.iter().flatten().find(|e| e.kind == EventKind::Input).unwrap().target, Some(instance(2)));
}

#[test]
fn cancelling_notification_keeps_the_game_button_pair_intact() {
    let original = table(InputMode::Notify);
    let mut router = Router::new();
    router.configure(&original);
    assert!(body(&mut router, 0, true, 20, 20).pass);
    let mut next = original.config().clone();
    next.revision = 2;
    next.widgets[0].visible = false;
    router.configure(&PreparedConfig::compile(next).unwrap());
    let e = body(&mut router, 0, false, 20, 20);
    assert!(e.pass);
    assert_eq!(input_count(&e), 0);
}

#[test]
fn global_edit_survives_hiding_focus_but_widget_takeover_does_not() {
    for takeover in [false, true] {
        let original = table(InputMode::Block);
        let mut router = Router::new();
        router.configure(&original);
        let r = if takeover {
            request(RoutingAction::Takeover, Some(instance(1)))
        } else {
            request(RoutingAction::EnterEdit, None)
        };
        router.request(r, context());
        body(&mut router, 0, true, 20, 20);
        body(&mut router, 0, false, 20, 20);
        let mut next = original.config().clone();
        next.revision = 2;
        next.widgets[0].visible = false;
        let e = router.configure(&PreparedConfig::compile(next).unwrap());
        assert_eq!(e.exclusive, !takeover);
    }
}

#[test]
fn unavailable_context_or_hidden_target_cannot_enter_exclusive_mode() {
    for condition in 0..3 {
        let mut router = Router::new();
        router.configure(&table(InputMode::Block));
        let mut c = context();
        match condition {
            0 => c.foreground = false,
            1 => c.visible = false,
            _ => c.healthy = false,
        }
        let e = router.request(request(RoutingAction::EnterEdit, None), c);
        assert!(!e.exclusive);
        assert_eq!(e.code, Some(RoutingCode::Unavailable));
    }
    let mut c = table(InputMode::Block).config().clone();
    c.widgets[0].visible = false;
    let mut router = Router::new();
    router.configure(&PreparedConfig::compile(c).unwrap());
    assert_eq!(
        router.request(request(RoutingAction::Takeover, Some(instance(1))), context()).code,
        Some(RoutingCode::Unavailable)
    );
}

#[test]
fn maximum_removal_fits_the_effect_budget_without_dropping_cancellation() {
    let mut c = table(InputMode::Block).config().clone();
    let prototype = c.widgets[0];
    c.widgets = (1..=32).map(|id| WidgetSpec { instance: instance(id), z: id as i32, ..prototype }).collect();
    let original = PreparedConfig::compile(c).unwrap();
    let mut router = Router::new();
    router.configure(&original);
    body(&mut router, 0, true, 20, 20);
    let mut c = original.config().clone();
    c.revision = 2;
    c.widgets.clear();
    let e = router.configure(&PreparedConfig::compile(c).unwrap());
    assert_eq!(e.code, Some(RoutingCode::Applied));
    assert_eq!(e.events.iter().flatten().filter(|e| e.kind == EventKind::Cancel).count(), 32);
}

#[test]
fn changing_client_geometry_releases_capture_and_does_not_rearm_hover() {
    let original = table(InputMode::Block);
    let mut router = Router::new();
    router.configure(&original);
    body(&mut router, 0, true, 20, 20);
    let mut c = original.config().clone();
    c.revision = 2;
    c.width = 1024;
    assert_eq!(router.configure(&PreparedConfig::compile(c).unwrap()).domains, Domains::default());
    assert_eq!(router.input(InputRecord::motion(20, 20, 0), context()).domains, Domains::default());
}

#[test]
fn a_released_key_from_a_cancelled_session_does_not_swallow_a_fresh_press() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Block));
    body(&mut router, 0, true, 20, 20);
    body(&mut router, 0, false, 20, 20);
    let mut c = context();
    c.physical_keys[87] = true;
    router.input(InputRecord::key(87, 0, true, false, false, 0), c);
    router.revoke(InputReason::FocusLost);
    router.request(request(RoutingAction::Takeover, Some(instance(1))), context());
    let e = router.input(InputRecord::key(87, 0, true, false, false, 0), context());
    assert_eq!(input_count(&e), 1);
}

#[test]
fn global_edit_entry_waits_for_a_widget_click_instead_of_retaining_standard_focus() {
    let mut router = Router::new();
    router.configure(&table(InputMode::Notify));
    body(&mut router, 0, true, 20, 20);
    body(&mut router, 0, false, 20, 20);
    router.request(request(RoutingAction::EnterEdit, None), context());
    assert_eq!(input_count(&router.input(InputRecord::text('x', 0), context())), 0);
}
