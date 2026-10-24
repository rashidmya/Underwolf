//! External input controller for the scratch Wine desktop; only targets the named test window.
#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() {
    use windows_sys::{
        Win32::UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
        core::w,
    };
    // SAFETY: static test-only window title; no arbitrary process target.
    let hwnd = unsafe { FindWindowW(std::ptr::null(), w!("uw-test-game")) };
    assert!(!hwnd.is_null(), "test window missing");
    // SAFETY: foreground only our scratch test fixture.
    unsafe {
        SetForegroundWindow(hwnd);
    }
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        let events = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 65, wScan: 30, dwFlags: 0, time: 0, dwExtraInfo: 0 } },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: 65, wScan: 30, dwFlags: KEYEVENTF_KEYUP, time: 0, dwExtraInfo: 0 },
                },
            },
        ];
        // SAFETY: initialized INPUT pair, native element size.
        assert_eq!(unsafe { SendInput(2, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) }, 2);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
