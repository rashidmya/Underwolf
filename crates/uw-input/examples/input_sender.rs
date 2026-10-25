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
    let arguments: Vec<_> = std::env::args().collect();
    if let Some(index) = arguments.iter().position(|argument| argument == "--control") {
        let action: usize = arguments.get(index + 1).expect("control number").parse().unwrap();
        assert!((1..=3).contains(&action));
        // SAFETY: numeric test-only message to the scratch window, with no cross-process pointers.
        assert_ne!(unsafe { PostMessageW(hwnd, WM_APP + 27, action, 0) }, 0);
        return;
    }
    let held_pulse = arguments.iter().any(|argument| argument == "--held-pulse");
    let paced = arguments.iter().any(|argument| argument == "--paced");
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
        if held_pulse {
            // SAFETY: one initialized down, then its matching up; the fixture always completes the pair.
            assert_eq!(unsafe { SendInput(1, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) }, 1);
            std::thread::sleep(std::time::Duration::from_millis(400));
            // SAFETY: second element is the matching initialized key-up.
            assert_eq!(unsafe { SendInput(1, events.as_ptr().add(1), std::mem::size_of::<INPUT>() as i32) }, 1);
            return;
        }
        // SAFETY: initialized down/up pair and the native element size.
        assert_eq!(unsafe { SendInput(2, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) }, 2);
        std::thread::sleep(std::time::Duration::from_millis(if paced { 30 } else { 1 }));
    }
}
