use uw_spike_input::capture_win32_hook::{translate_message, wm, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use uw_spike_input::gate::RawInput;

fn lparam_xy(x: i16, y: i16) -> isize {
    (((y as u16 as u32) << 16) | (x as u16 as u32)) as i32 as isize
}

fn wparam_wheel(delta: i16) -> usize {
    ((delta as u16 as u32) << 16) as usize
}

#[test]
fn translates_key_down_and_up() {
    assert_eq!(
        translate_message(wm::WM_KEYDOWN, b'W' as usize, 0),
        Some(RawInput::Key { code: b'W' as u32, down: true, text: None })
    );
    assert_eq!(
        translate_message(wm::WM_KEYUP, b'W' as usize, 0),
        Some(RawInput::Key { code: b'W' as u32, down: false, text: None })
    );
}

#[test]
fn translates_syskey_as_key() {
    // Alt-combos arrive as WM_SYSKEY*; the game still needs them tracked.
    assert_eq!(
        translate_message(wm::WM_SYSKEYDOWN, 0x12, 0),
        Some(RawInput::Key { code: 0x12, down: true, text: None })
    );
    assert_eq!(
        translate_message(wm::WM_SYSKEYUP, 0x12, 0),
        Some(RawInput::Key { code: 0x12, down: false, text: None })
    );
}

#[test]
fn translates_mouse_buttons() {
    assert_eq!(
        translate_message(wm::WM_LBUTTONDOWN, 0, 0),
        Some(RawInput::MouseButton { button: BTN_LEFT, down: true })
    );
    assert_eq!(
        translate_message(wm::WM_LBUTTONUP, 0, 0),
        Some(RawInput::MouseButton { button: BTN_LEFT, down: false })
    );
    assert_eq!(
        translate_message(wm::WM_RBUTTONDOWN, 0, 0),
        Some(RawInput::MouseButton { button: BTN_RIGHT, down: true })
    );
    assert_eq!(
        translate_message(wm::WM_MBUTTONDOWN, 0, 0),
        Some(RawInput::MouseButton { button: BTN_MIDDLE, down: true })
    );
}

#[test]
fn translates_mouse_move_with_signed_coords() {
    assert_eq!(
        translate_message(wm::WM_MOUSEMOVE, 0, lparam_xy(100, 50)),
        Some(RawInput::MouseMove { x: 100.0, y: 50.0 })
    );
    assert_eq!(
        translate_message(wm::WM_MOUSEMOVE, 0, lparam_xy(-3, -7)),
        Some(RawInput::MouseMove { x: -3.0, y: -7.0 })
    );
}

#[test]
fn translates_wheel_in_notches() {
    assert_eq!(
        translate_message(wm::WM_MOUSEWHEEL, wparam_wheel(120), 0),
        Some(RawInput::Wheel { dx: 0.0, dy: 1.0 })
    );
    assert_eq!(
        translate_message(wm::WM_MOUSEWHEEL, wparam_wheel(-120), 0),
        Some(RawInput::Wheel { dx: 0.0, dy: -1.0 })
    );
    assert_eq!(
        translate_message(wm::WM_MOUSEHWHEEL, wparam_wheel(120), 0),
        Some(RawInput::Wheel { dx: 1.0, dy: 0.0 })
    );
}

#[test]
fn ignores_unhandled_messages() {
    // WM_PAINT and friends are not input and must be left alone.
    assert_eq!(translate_message(0x000F, 0, 0), None);
}
