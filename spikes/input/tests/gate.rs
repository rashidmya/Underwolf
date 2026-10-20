use uw_spike_input::gate::{vk, GameAction, InputGate, RawInput};
use uw_spike_ipc::InputEvent;

fn key_down(code: u32) -> RawInput {
    RawInput::Key { code, down: true, text: None }
}
fn key_up(code: u32) -> RawInput {
    RawInput::Key { code, down: false, text: None }
}

#[test]
fn outside_edit_mode_passes_keys_to_game_and_forwards_nothing() {
    let mut g = InputGate::new();
    let s = g.step(key_down(b'W' as u32));
    assert_eq!(s.to_game, vec![GameAction::Pass(key_down(b'W' as u32))]);
    assert!(s.to_host.is_empty());
    assert_eq!(s.edit_mode_changed, None);
    assert_eq!(g.game_held_keys(), &[b'W' as u32]);
}

#[test]
fn held_key_is_forgotten_on_key_up() {
    let mut g = InputGate::new();
    g.step(key_down(b'W' as u32));
    g.step(key_up(b'W' as u32));
    assert!(g.game_held_keys().is_empty());
}

#[test]
fn tab_without_ctrl_passes_through_and_does_not_toggle() {
    let mut g = InputGate::new();
    let s = g.step(key_down(vk::TAB));
    assert_eq!(s.edit_mode_changed, None);
    assert!(!g.edit_mode());
    assert_eq!(s.to_game, vec![GameAction::Pass(key_down(vk::TAB))]);
}

#[test]
fn ctrl_tab_enters_edit_mode() {
    let mut g = InputGate::new();
    g.step(key_down(vk::LCONTROL));
    let s = g.step(key_down(vk::TAB));
    assert_eq!(s.edit_mode_changed, Some(true));
    assert!(g.edit_mode());
    // The Tab that triggered the toggle must not reach the game.
    assert!(!s.to_game.contains(&GameAction::Pass(key_down(vk::TAB))));
    // The hotkey is consumed, not forwarded as a host input event.
    assert!(s.to_host.is_empty());
}

#[test]
fn entering_edit_mode_releases_every_held_game_key() {
    let mut g = InputGate::new();
    g.step(key_down(b'W' as u32)); // moving
    g.step(key_down(vk::LCONTROL)); // crouch (also the chord modifier)
    let s = g.step(key_down(vk::TAB)); // enter

    assert_eq!(s.edit_mode_changed, Some(true));
    assert!(s.to_game.contains(&GameAction::SynthKeyUp(b'W' as u32)));
    assert!(s.to_game.contains(&GameAction::SynthKeyUp(vk::LCONTROL)));
    assert!(g.game_held_keys().is_empty());
}

#[test]
fn entering_edit_mode_releases_held_mouse_button() {
    let mut g = InputGate::new();
    g.step(RawInput::MouseButton { button: 0, down: true });
    g.step(key_down(vk::LCONTROL));
    let s = g.step(key_down(vk::TAB));
    assert!(s.to_game.contains(&GameAction::SynthButtonUp(0)));
    assert!(g.game_held_buttons().is_empty());
}

#[test]
fn edit_mode_swallows_input_from_game_and_forwards_to_host() {
    let mut g = InputGate::new();
    g.step(key_down(vk::LCONTROL));
    g.step(key_down(vk::TAB)); // now in edit mode

    let s = g.step(RawInput::MouseMove { x: 100.0, y: 50.0 });
    assert!(s.to_game.is_empty(), "game must see no mouse-look in edit mode");
    assert_eq!(s.to_host, vec![InputEvent::MouseMove { x: 100.0, y: 50.0 }]);

    let s = g.step(key_down(b'W' as u32));
    assert!(s.to_game.is_empty());
    assert_eq!(
        s.to_host,
        vec![InputEvent::Key { code: b'W' as u32, down: true, text: None }]
    );
    // Keys pressed in edit mode never become game-held.
    assert!(g.game_held_keys().is_empty());
}

#[test]
fn ctrl_tab_exits_edit_mode() {
    let mut g = InputGate::new();
    g.step(key_down(vk::LCONTROL));
    g.step(key_down(vk::TAB)); // enter
    g.step(key_up(vk::TAB)); // tapped, not held
    g.step(key_up(vk::LCONTROL)); // released inside edit mode
    g.step(key_down(vk::LCONTROL)); // exit chord
    let s = g.step(key_down(vk::TAB));
    assert_eq!(s.edit_mode_changed, Some(false));
    assert!(!g.edit_mode());
}

#[test]
fn held_tab_autorepeat_does_not_double_toggle() {
    let mut g = InputGate::new();
    g.step(key_down(vk::LCONTROL));
    let s1 = g.step(key_down(vk::TAB)); // enter on the rising edge
    assert_eq!(s1.edit_mode_changed, Some(true));

    // The player keeps Ctrl+Tab held; Win32 delivers WM_KEYDOWN auto-repeats.
    let s2 = g.step(key_down(vk::TAB));
    let s3 = g.step(key_down(vk::TAB));
    assert_eq!(s2.edit_mode_changed, None, "auto-repeat must not re-toggle");
    assert_eq!(s3.edit_mode_changed, None);
    assert!(g.edit_mode(), "still in edit mode after held Ctrl+Tab");
}

/// Review Focus 3 / P5: a full enter→exit cycle, with the player still holding
/// movement and crouch physically, must leave the game with nothing stuck down.
#[test]
fn exit_leaves_no_keys_held_in_game() {
    let mut g = InputGate::new();
    // Player is moving and about to open edit mode.
    g.step(key_down(b'W' as u32));
    g.step(key_down(vk::LCONTROL));
    g.step(key_down(vk::TAB)); // ENTER — releases W and Ctrl to the game
    g.step(key_up(vk::TAB)); // chord was a tap
    g.step(key_up(vk::LCONTROL));

    assert!(g.game_held_keys().is_empty(), "entry must clear game-held keys");

    // Inside edit mode the player wiggles and types; none of it reaches the game.
    g.step(RawInput::MouseMove { x: 10.0, y: 10.0 });
    g.step(key_down(b'A' as u32));
    g.step(key_up(b'A' as u32));

    // Exit chord — player presses Ctrl+Tab again while still holding W physically.
    g.step(key_down(vk::LCONTROL));
    let s = g.step(key_down(vk::TAB)); // EXIT

    assert_eq!(s.edit_mode_changed, Some(false));
    assert!(
        g.game_held_keys().is_empty(),
        "after exit the game must believe no key is held (not crouched, no W repeat)"
    );
    assert!(g.game_held_buttons().is_empty());
}
