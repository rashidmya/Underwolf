//! The raw-input core (C3's answer to the P2 camera leak). Source 2 reads mouse-look through
//! `GetRawInputData`, which the WndProc subclass never sees. These tests pin the RAWINPUT parsing
//! and the in-place neutralization that stops the camera while edit mode is on.

use uw_spike_input::capture_win32_hook::{
    BTN_LEFT, BTN_RIGHT, HostSink, handle_raw_mouse, neutralize_raw_mouse, parse_raw_mouse, raw_mouse_extra_info,
};
use uw_spike_input::gate::{InputGate, RawInput, vk};
use uw_spike_ipc::InputEvent;

#[derive(Default)]
struct FakeHost {
    inputs: Vec<InputEvent>,
    edit_changes: Vec<bool>,
}
impl HostSink for FakeHost {
    fn input(&mut self, event: &InputEvent) {
        self.inputs.push(event.clone());
    }
    fn edit_mode_changed(&mut self, on: bool) {
        self.edit_changes.push(on);
    }
}

fn enter_edit_mode(gate: &mut InputGate) {
    gate.step(RawInput::Key { code: vk::LCONTROL, down: true, text: None });
    gate.step(RawInput::Key { code: vk::TAB, down: true, text: None });
    assert!(gate.edit_mode());
}

const RI_MOUSE_LEFT_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_DOWN: u16 = 0x0004;
const RI_MOUSE_WHEEL: u16 = 0x0400;
const RI_MOUSE_HWHEEL: u16 = 0x0800;

/// Build a 48-byte x86_64 RAWINPUT holding one RAWMOUSE record (header type 0 = mouse).
fn raw_mouse(button_flags: u16, button_data: i16, dx: i32, dy: i32) -> Vec<u8> {
    let mut b = vec![0u8; 48];
    // dwType @0 = 0 (RIM_TYPEMOUSE), already zero. RAWMOUSE starts at offset 24.
    b[28..30].copy_from_slice(&button_flags.to_le_bytes()); // usButtonFlags
    b[30..32].copy_from_slice(&(button_data as u16).to_le_bytes()); // usButtonData (wheel)
    b[36..40].copy_from_slice(&dx.to_le_bytes()); // lLastX
    b[40..44].copy_from_slice(&dy.to_le_bytes()); // lLastY
    b
}

#[test]
fn parses_relative_motion() {
    assert_eq!(parse_raw_mouse(&raw_mouse(0, 0, 5, -3)), vec![RawInput::MouseMove { x: 5.0, y: -3.0 }]);
}

#[test]
fn skips_a_zero_motion_record() {
    assert!(parse_raw_mouse(&raw_mouse(0, 0, 0, 0)).is_empty());
}

#[test]
fn parses_button_down_and_up() {
    assert_eq!(
        parse_raw_mouse(&raw_mouse(RI_MOUSE_RIGHT_DOWN, 0, 0, 0)),
        vec![RawInput::MouseButton { button: BTN_RIGHT, down: true }]
    );
    assert_eq!(
        parse_raw_mouse(&raw_mouse(RI_MOUSE_LEFT_UP, 0, 0, 0)),
        vec![RawInput::MouseButton { button: BTN_LEFT, down: false }]
    );
}

#[test]
fn motion_and_button_in_one_record_are_both_reported() {
    assert_eq!(
        parse_raw_mouse(&raw_mouse(RI_MOUSE_LEFT_DOWN, 0, 2, 2)),
        vec![RawInput::MouseMove { x: 2.0, y: 2.0 }, RawInput::MouseButton { button: BTN_LEFT, down: true }]
    );
}

#[test]
fn parses_wheel_in_notches() {
    assert_eq!(parse_raw_mouse(&raw_mouse(RI_MOUSE_WHEEL, 120, 0, 0)), vec![RawInput::Wheel { dx: 0.0, dy: 1.0 }]);
    assert_eq!(parse_raw_mouse(&raw_mouse(RI_MOUSE_HWHEEL, -120, 0, 0)), vec![RawInput::Wheel { dx: -1.0, dy: 0.0 }]);
}

#[test]
fn ignores_non_mouse_records() {
    let mut b = vec![0u8; 48];
    b[0..4].copy_from_slice(&1u32.to_le_bytes()); // dwType = RIM_TYPEKEYBOARD
    assert!(parse_raw_mouse(&b).is_empty());
}

#[test]
fn neutralized_record_carries_no_input() {
    let mut b = raw_mouse(RI_MOUSE_LEFT_DOWN, 120, 9, -9);
    neutralize_raw_mouse(&mut b);
    assert!(parse_raw_mouse(&b).is_empty(), "the game must read no motion, button or wheel");
    assert_eq!(&b[36..40], &0i32.to_le_bytes());
    assert_eq!(&b[40..44], &0i32.to_le_bytes());
}

#[test]
fn neutralize_leaves_a_non_mouse_record_untouched() {
    let mut b = vec![0u8; 48];
    b[0..4].copy_from_slice(&1u32.to_le_bytes()); // keyboard
    b[36..40].copy_from_slice(&42i32.to_le_bytes());
    neutralize_raw_mouse(&mut b);
    assert_eq!(&b[36..40], &42i32.to_le_bytes());
}

#[test]
fn reads_the_extra_info_tag() {
    let mut b = raw_mouse(0, 0, 1, 1);
    b[44..48].copy_from_slice(&0x5557_4F4Cu32.to_le_bytes()); // ulExtraInformation
    assert_eq!(raw_mouse_extra_info(&b), Some(0x5557_4F4C));
    let mut kbd = vec![0u8; 48];
    kbd[0..4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(raw_mouse_extra_info(&kbd), None);
}

#[test]
fn raw_mouse_passes_through_the_game_outside_edit_mode() {
    let mut gate = InputGate::new();
    let mut host = FakeHost::default();
    let mut b = raw_mouse(0, 0, 5, 5);
    handle_raw_mouse(&mut b, &mut gate, &mut host);
    assert!(host.inputs.is_empty(), "nothing is forwarded while not editing");
    assert_eq!(parse_raw_mouse(&b), vec![RawInput::MouseMove { x: 5.0, y: 5.0 }], "record left intact for the game");
}

#[test]
fn raw_mouse_is_blocked_from_the_game_and_forwarded_in_edit_mode() {
    let mut gate = InputGate::new();
    let mut host = FakeHost::default();
    enter_edit_mode(&mut gate);

    let mut b = raw_mouse(0, 0, 5, -5);
    handle_raw_mouse(&mut b, &mut gate, &mut host);

    assert_eq!(host.inputs, vec![InputEvent::MouseMove { x: 5.0, y: -5.0 }], "overlay sees the motion");
    assert!(parse_raw_mouse(&b).is_empty(), "the game's camera must not move (P2)");
}
