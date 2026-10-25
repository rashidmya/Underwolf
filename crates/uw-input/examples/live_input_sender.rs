//! One-shot OS input fixture for an owner-observed Hideout check. Does not change focus.
#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() {
    use windows_sys::{
        Win32::{
            Foundation::{POINT, RECT},
            Graphics::Gdi::ClientToScreen,
            UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
        },
        core::w,
    };
    // SAFETY: exact test target title, within the selected Wine session.
    let hwnd = unsafe { FindWindowW(std::ptr::null(), w!("Deadlock")) };
    assert!(!hwnd.is_null(), "Deadlock window missing");
    let foreground = || {
        // SAFETY: scalar queries on the opaque target handle.
        unsafe { IsWindow(hwnd) != 0 && GetForegroundWindow() == hwnd }
    };
    assert!(foreground(), "Deadlock must already be foreground");
    let send = |events: &[INPUT]| {
        assert!(foreground(), "foreground changed; cancelling remaining fixture input");
        // SAFETY: initialized INPUT array and native element size.
        let inserted = unsafe { SendInput(events.len() as u32, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if inserted != events.len() as u32 {
            // Serialized input is not transactional. Attempt every matching up even if
            // focus changed; never send another down after a partial insertion.
            let releases: Vec<INPUT> = events
                .iter()
                .filter(|event| {
                    // SAFETY: each initialized union member is selected by its INPUT type.
                    unsafe {
                        match event.r#type {
                            INPUT_KEYBOARD => event.Anonymous.ki.dwFlags & KEYEVENTF_KEYUP != 0,
                            INPUT_MOUSE => {
                                event.Anonymous.mi.dwFlags
                                    & (MOUSEEVENTF_LEFTUP
                                        | MOUSEEVENTF_MIDDLEUP
                                        | MOUSEEVENTF_RIGHTUP
                                        | MOUSEEVENTF_XUP)
                                    != 0
                            }
                            _ => false,
                        }
                    }
                })
                .copied()
                .collect();
            if !releases.is_empty() {
                let mut released = 0;
                for _ in 0..3 {
                    // SAFETY: initialized release-only suffix and native element size.
                    released += unsafe {
                        SendInput(
                            (releases.len() - released) as u32,
                            releases[released..].as_ptr(),
                            std::mem::size_of::<INPUT>() as i32,
                        ) as usize
                    };
                    if released == releases.len() {
                        break;
                    }
                }
                eprintln!("Partial input: release cleanup inserted {released}/{} up events", releases.len());
            }
            eprintln!(
                "ABORTED: SendInput inserted {inserted}/{} events; release physical keys/buttons before continuing",
                events.len()
            );
            std::process::exit(2);
        }
    };
    if std::env::args().any(|arg| arg == "--hotkey-only") {
        let events = [(VK_LCONTROL, 0), (VK_TAB, 0), (VK_TAB, KEYEVENTF_KEYUP), (VK_LCONTROL, KEYEVENTF_KEYUP)].map(
            |(key, flags)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            },
        );
        send(&events);
        println!("SENT: one Ctrl+Tab chord; verify actual capture state separately.");
        return;
    }
    let mut rect = RECT::default();
    // SAFETY: writable rectangle for the known target window.
    assert_ne!(unsafe { GetClientRect(hwnd, &mut rect) }, 0);
    let mut point = POINT { x: ((rect.right - rect.left) / 2).min(300), y: ((rect.bottom - rect.top) / 2).min(300) };
    assert!(foreground(), "foreground changed; cancelling pointer placement");
    // SAFETY: writable point; keep the pointer well inside the game client area.
    unsafe {
        assert_ne!(ClientToScreen(hwnd, &mut point), 0);
        assert_ne!(SetCursorPos(point.x, point.y), 0);
    }
    let mouse = |flags, data| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let mut text = Vec::new();
    for unit in "3a😀".encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            text.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
            });
        }
    }
    send(&text);
    for (down, up, data) in [
        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
        (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
    ] {
        send(&[mouse(down, data), mouse(up, data)]);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    send(&[mouse(MOUSEEVENTF_WHEEL, 120), mouse(MOUSEEVENTF_HWHEEL, 120)]);
    println!(
        "SENT: Unicode 3a😀, five button pairs, vertical and horizontal wheel. Verify receiver records and owner observations separately."
    );
}
