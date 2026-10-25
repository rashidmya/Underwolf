use uw_pipe_proto::routing::*;

fn config() -> RoutingConfig {
    RoutingConfig {
        epoch: 1,
        revision: 1,
        request_id: 1,
        width: 800,
        height: 600,
        widgets: vec![WidgetSpec {
            instance: WidgetRef { id: 1, generation: 1 },
            x: -20,
            y: 0,
            width: 100,
            height: 100,
            z: 1,
            mode: InputMode::Block,
            visible: true,
            keyboard: true,
        }],
        hotkeys: vec![HotkeySpec {
            id: 1,
            vk: 9,
            modifiers: 1 << 3,
            flags: 0,
            action: HotkeyAction::ToggleEdit,
            target: None,
        }],
    }
}

#[test]
fn overlapping_sided_and_unsided_chords_cannot_compete() {
    let mut c = config();
    let mut chord = c.hotkeys[0];
    chord.id = 2;
    chord.modifiers = 2 << 3;
    chord.action = HotkeyAction::Notify;
    c.hotkeys.push(chord);
    assert!(c.validate().is_err());
}

#[test]
fn extra_modifiers_are_part_of_conflict_detection() {
    let mut c = config();
    c.hotkeys[0].flags = 2;
    let mut second = c.hotkeys[0];
    second.id = 2;
    second.flags = 0;
    second.modifiers |= 1;
    second.action = HotkeyAction::Notify;
    c.hotkeys.push(second);
    assert!(c.validate().is_err());
}

#[test]
fn replacing_a_widget_slot_requires_a_new_configuration_instance() {
    let mut c = config();
    let mut other = c.widgets[0];
    other.instance.generation = 2;
    other.z = 2;
    c.widgets.push(other);
    assert!(c.validate().is_err());
}

#[test]
fn takeover_cannot_name_an_unknown_generation() {
    let mut c = config();
    c.hotkeys.push(HotkeySpec {
        id: 2,
        vk: 121,
        modifiers: 0,
        flags: 0,
        action: HotkeyAction::Takeover,
        target: Some(WidgetRef { id: 1, generation: 2 }),
    });
    assert!(c.validate().is_err());
}

#[test]
fn clipped_and_hidden_targets_remain_valid_but_invalid_geometry_does_not() {
    let mut c = config();
    assert!(c.validate().is_ok());
    c.widgets[0].visible = false;
    c.widgets[0].x = i32::MAX;
    assert!(c.validate().is_ok());
    c.widgets[0].width = -1;
    assert!(c.validate().is_err());
}

#[test]
fn every_configuration_preserves_one_exit_chord() {
    let mut c = config();
    c.hotkeys.clear();
    assert!(c.validate().is_err());
    c.hotkeys = config().hotkeys;
    let mut second = c.hotkeys[0];
    second.id = 2;
    second.vk = 121;
    c.hotkeys.push(second);
    assert!(c.validate().is_err());
}

#[test]
fn duplicate_z_and_zero_generations_are_rejected() {
    let mut c = config();
    let mut second = c.widgets[0];
    second.instance.id = 2;
    c.widgets.push(second);
    assert!(c.validate().is_err());
    c.widgets.pop();
    c.widgets[0].instance.generation = 0;
    assert!(c.validate().is_err());
}
