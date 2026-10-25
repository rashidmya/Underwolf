use uw_input::{configuration::PreparedConfig, hotkeys::HotkeyMatcher};
use uw_pipe_proto::{input::InputRecord, routing::*};

fn table(vk: u16, modifiers: u16, flags: u16) -> PreparedConfig {
    PreparedConfig::compile(RoutingConfig {
        epoch: 1,
        revision: 1,
        request_id: 1,
        width: 800,
        height: 600,
        widgets: vec![],
        hotkeys: vec![HotkeySpec { id: 1, vk, modifiers, flags, action: HotkeyAction::ToggleEdit, target: None }],
    })
    .unwrap()
}
fn key(vk: i32, down: bool, repeat: bool) -> InputRecord {
    InputRecord::key(vk, 0, down, false, repeat, 0)
}
fn ready(config: &PreparedConfig) -> HotkeyMatcher {
    let mut matcher = HotkeyMatcher::default();
    matcher.replace(config, &[false; 256]);
    matcher
}

#[test]
fn either_ctrl_side_matches_real_state_even_when_record_modifiers_are_zero() {
    for ctrl in [162, 163] {
        let mut matcher = ready(&table(9, 8, 0));
        let mut keys = [false; 256];
        keys[ctrl] = true;
        keys[9] = true;
        let hit = matcher.observe(key(9, true, false), &keys).unwrap();
        assert_eq!(hit.binding_id, 1);
        assert_eq!(hit.action, HotkeyAction::ToggleEdit);
        assert!(matcher.consumes_last());
    }
}

#[test]
fn repeats_and_duplicate_raw_legacy_edges_fire_once() {
    let mut matcher = ready(&table(9, 8, 0));
    let mut keys = [false; 256];
    keys[162] = true;
    keys[9] = true;
    assert!(matcher.observe(key(9, true, false), &keys).is_some());
    assert!(matcher.observe(key(9, true, false), &keys).is_none());
    assert!(matcher.observe(key(9, true, true), &keys).is_none());
    assert!(matcher.consumes_last());
    keys[9] = false;
    assert!(matcher.observe(key(9, false, false), &keys).is_none());
    assert!(matcher.consumes_last());
    keys[9] = true;
    assert!(matcher.observe(key(9, true, false), &keys).is_some());
}

#[test]
fn extra_shift_or_altgr_does_not_match_plain_ctrl_chord() {
    for extra in [160, 165] {
        let mut matcher = ready(&table(9, 8, 0));
        let mut keys = [false; 256];
        keys[162] = true;
        keys[extra] = true;
        keys[9] = true;
        assert!(matcher.observe(key(9, true, false), &keys).is_none());
        assert!(!matcher.consumes_last());
    }
}

#[test]
fn allowed_extra_modifiers_and_explicit_ctrl_alt_are_distinct() {
    let mut keys = [false; 256];
    keys[162] = true;
    keys[165] = true;
    keys[9] = true;
    assert!(ready(&table(9, 8, 2)).observe(key(9, true, false), &keys).is_some());
    assert!(ready(&table(9, 8 | 64, 0)).observe(key(9, true, false), &keys).is_some());
}

#[test]
fn sided_binding_rejects_the_other_side() {
    let mut keys = [false; 256];
    keys[163] = true;
    keys[9] = true;
    assert!(ready(&table(9, 16, 0)).observe(key(9, true, false), &keys).is_none());
    keys[163] = false;
    keys[162] = true;
    assert!(ready(&table(9, 16, 0)).observe(key(9, true, false), &keys).is_some());
}

#[test]
fn replacing_a_table_while_its_trigger_is_held_requires_a_fresh_press() {
    let mut matcher = HotkeyMatcher::default();
    let mut keys = [false; 256];
    keys[162] = true;
    keys[121] = true;
    matcher.replace(&table(121, 8, 0), &keys);
    assert!(matcher.observe(key(121, true, false), &keys).is_none());
    keys[121] = false;
    matcher.observe(key(121, false, false), &keys);
    keys[121] = true;
    assert!(matcher.observe(key(121, true, false), &keys).is_some());
}

#[test]
fn replacing_a_binding_preserves_its_already_consumed_key_up() {
    let mut matcher = ready(&table(9, 8, 0));
    let mut keys = [false; 256];
    keys[162] = true;
    keys[9] = true;
    matcher.observe(key(9, true, false), &keys);
    matcher.replace(&table(121, 8, 0), &keys);
    keys[9] = false;
    assert!(matcher.observe(key(9, false, false), &keys).is_none());
    assert!(matcher.consumes_last());
    keys[9] = true;
    assert!(matcher.observe(key(9, true, false), &keys).is_none());
    assert!(!matcher.consumes_last());
}

#[test]
fn passthrough_notification_never_consumes_either_edge() {
    let mut config = table(9, 8, 0).config().clone();
    config.hotkeys.push(HotkeySpec {
        id: 2,
        vk: 121,
        modifiers: 0,
        flags: 1,
        action: HotkeyAction::Notify,
        target: None,
    });
    let mut matcher = ready(&PreparedConfig::compile(config).unwrap());
    let mut keys = [false; 256];
    keys[121] = true;
    let hit = matcher.observe(key(121, true, false), &keys).unwrap();
    assert!(hit.passthrough);
    assert!(!matcher.consumes_last());
    keys[121] = false;
    matcher.observe(key(121, false, false), &keys);
    assert!(!matcher.consumes_last());
}

#[test]
fn a_repeat_without_its_original_down_cannot_activate() {
    let mut matcher = ready(&table(9, 8, 0));
    let mut keys = [false; 256];
    keys[162] = true;
    keys[9] = true;
    assert!(matcher.observe(key(9, true, true), &keys).is_none());
    assert!(matcher.observe(key(9, true, false), &keys).is_none());
}

#[test]
fn a_both_sides_requirement_does_not_mean_either_side() {
    let mut keys = [false; 256];
    keys[162] = true;
    keys[9] = true;
    assert!(ready(&table(9, 48, 0)).observe(key(9, true, false), &keys).is_none());
    keys[163] = true;
    assert!(ready(&table(9, 48, 0)).observe(key(9, true, false), &keys).is_some());
    assert!(ready(&table(9, 8, 0)).observe(key(9, true, false), &keys).is_some());
}
