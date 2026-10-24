//! Cursor originals and game-visible intent. Restoration runs on the captured GUI thread.
use super::*;
use crate::cursor::{CursorAction, CursorLedger, CursorSnapshot, Point, Rect, RestorePlan};
use windows_sys::Win32::{
    Foundation::{GetLastError, POINT, RECT, SetLastError},
    System::Threading::GetCurrentThreadId,
    UI::{Input::KeyboardAndMouse::GetFocus, WindowsAndMessaging::*},
};
static GET_POS: AtomicUsize = AtomicUsize::new(0);
static SET_POS: AtomicUsize = AtomicUsize::new(0);
static CLIP: AtomicUsize = AtomicUsize::new(0);
static GET_CLIP: AtomicUsize = AtomicUsize::new(0);
static SHOW: AtomicUsize = AtomicUsize::new(0);
static SET_SHAPE: AtomicUsize = AtomicUsize::new(0);
static GET_SHAPE: AtomicUsize = AtomicUsize::new(0);
static RESTORE: Mutex<Option<RestorePlan>> = Mutex::new(None);
static CURSOR: Mutex<Option<CursorLedger>> = Mutex::new(None);
macro_rules! original {
    ($slot:ident,$ty:ty) => {{
        // SAFETY: each slot is initialized with the matching API trampoline before enabling.
        unsafe { std::mem::transmute::<usize, $ty>($slot.load(Ordering::Acquire)) }
    }};
}
pub(super) fn targets() -> Vec<hooks::Target> {
    vec![
        hooks::Target {
            address: GetCursorPos as *const () as usize,
            detour: get_pos as *const () as usize,
            original: &GET_POS,
        },
        hooks::Target {
            address: SetCursorPos as *const () as usize,
            detour: set_pos as *const () as usize,
            original: &SET_POS,
        },
        hooks::Target {
            address: ClipCursor as *const () as usize,
            detour: clip as *const () as usize,
            original: &CLIP,
        },
        hooks::Target {
            address: GetClipCursor as *const () as usize,
            detour: get_clip as *const () as usize,
            original: &GET_CLIP,
        },
        hooks::Target {
            address: ShowCursor as *const () as usize,
            detour: show as *const () as usize,
            original: &SHOW,
        },
        hooks::Target {
            address: SetCursor as *const () as usize,
            detour: set_shape as *const () as usize,
            original: &SET_SHAPE,
        },
        hooks::Target {
            address: GetCursor as *const () as usize,
            detour: get_shape as *const () as usize,
            original: &GET_SHAPE,
        },
    ]
}
fn captured() -> bool {
    !bypassed() && SHARED.get().is_some_and(|s| s.capture())
}
// ShowCursor and cursor shape belong to an input queue, not the process.
// Attached threads share the focused window and the owner's cursor count.
fn owner_queue() -> bool {
    SHARED.get().is_some_and(|s| {
        // SAFETY: thread-local queries take no pointer arguments.
        unsafe { GetCurrentThreadId() == s.thread || GetFocus() as usize == s.hwnd }
    })
}
fn access<T>(f: impl FnOnce(&mut CursorLedger) -> T) -> Option<T> {
    if let Ok(mut slot) = CURSOR.try_lock() {
        return slot.as_mut().map(f);
    }
    if let Some(s) = SHARED.get() {
        s.revoke(InputReason::Unavailable)
    }
    None
}
pub(super) fn real_position() -> Option<POINT> {
    let mut p = POINT::default();
    let f = original!(GET_POS, unsafe extern "system" fn(*mut POINT) -> i32);
    // SAFETY: valid writable point and published trampoline.
    (unsafe { f(&mut p) } != 0).then_some(p)
}
pub(super) fn enter() -> bool {
    let Some(position) = real_position() else { return false };
    let mut clip = RECT::default();
    let get = original!(GET_CLIP, unsafe extern "system" fn(*mut RECT) -> i32);
    let show = original!(SHOW, unsafe extern "system" fn(i32) -> i32);
    let shape = original!(GET_SHAPE, unsafe extern "system" fn() -> HCURSOR);
    let set = original!(SET_SHAPE, unsafe extern "system" fn(HCURSOR) -> HCURSOR);
    let free = original!(CLIP, unsafe extern "system" fn(*const RECT) -> i32);
    // SAFETY: valid local rectangle; cursor calls are made on the GUI owner thread.
    unsafe {
        if get(&mut clip) == 0 {
            return false;
        }
        let old_shape = shape();
        let baseline = show(1).saturating_sub(1);
        show(0);
        let mut adjustment = 0;
        let mut current = baseline;
        while current < 0 && adjustment < 64 {
            current = show(1);
            adjustment += 1
        }
        if current < 0 {
            for _ in 0..adjustment {
                show(0);
            }
            return false;
        }
        if free(std::ptr::null()) == 0 {
            for _ in 0..adjustment {
                show(0);
            }
            return false;
        }
        let arrow = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        if arrow.is_null() {
            for _ in 0..adjustment {
                show(0);
            }
            free(&clip);
            return false;
        }
        set(arrow);
        let ledger = CursorLedger::new(
            CursorSnapshot {
                position: Point { x: position.x, y: position.y },
                clip: Some(Rect { left: clip.left, top: clip.top, right: clip.right, bottom: clip.bottom }),
                shape: old_shape as usize,
                display_count: baseline,
            },
            adjustment,
        );
        if let Ok(mut slot) = CURSOR.try_lock() {
            *slot = Some(ledger);
            true
        } else {
            for _ in 0..adjustment {
                show(0);
            }
            free(&clip);
            set(old_shape);
            false
        }
    }
}
pub(super) fn restore(foreground: bool, alive: bool) -> bool {
    let Ok(mut pending) = RESTORE.try_lock() else { return false };
    if pending.is_none() {
        let Ok(mut slot) = CURSOR.try_lock() else { return false };
        let Some(mut ledger) = slot.take() else { return true };
        *pending = Some(RestorePlan::new(ledger.release(foreground, alive)));
    }
    let plan = pending.as_mut().unwrap();
    let pos = original!(SET_POS, unsafe extern "system" fn(i32, i32) -> i32);
    let clip = original!(CLIP, unsafe extern "system" fn(*const RECT) -> i32);
    let shape = original!(SET_SHAPE, unsafe extern "system" fn(HCURSOR) -> HCURSOR);
    let show = original!(SHOW, unsafe extern "system" fn(i32) -> i32);
    while let Some(action) = plan.next(foreground, alive) {
        // SAFETY: actions contain saved scalar intent; all calls run on the owner thread.
        let success = unsafe {
            match action {
                CursorAction::Warp(p) => pos(p.x, p.y) != 0,
                CursorAction::Clip(r) => {
                    let r = r.map(|r| RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom });
                    clip(r.as_ref().map_or(std::ptr::null(), |r| r)) != 0
                }
                CursorAction::Shape(s) => {
                    shape(s as HCURSOR);
                    true
                }
                CursorAction::Balance(n) => {
                    for _ in 0..n.unsigned_abs() {
                        show((n > 0) as i32);
                    }
                    true
                }
            }
        };
        if !success {
            return false;
        }
        plan.complete();
        // Yield between large count adjustments; the next owner wake resumes progress.
        if matches!(action, CursorAction::Balance(n) if n.unsigned_abs() == 4096) {
            return false;
        }
    }
    *pending = None;
    true
}
unsafe extern "system" fn get_pos(out: *mut POINT) -> i32 {
    boundary(0, || {
        let f = original!(GET_POS, unsafe extern "system" fn(*mut POINT) -> i32);
        // SAFETY: forwards caller pointer; original validates it before we replace successful output.
        let result = unsafe { f(out) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0
            && captured()
            && let Some(p) = access(|l| l.frozen)
        {
            // SAFETY: successful original wrote the supplied POINT.
            unsafe { *out = POINT { x: p.x, y: p.y } }
        }
        // SAFETY: retain original error code.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn set_pos(x: i32, y: i32) -> i32 {
    boundary(0, || {
        let f = original!(SET_POS, unsafe extern "system" fn(i32, i32) -> i32);
        let active = captured();
        let before = if active { real_position() } else { None };
        // SAFETY: scalar API arguments forwarded unchanged.
        let result = unsafe { f(x, y) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0 && active {
            access(|l| l.position(Point { x, y }));
            if let Some(p) = before {
                // SAFETY: restore the real overlay pointer after validating the game's request with original API.
                unsafe {
                    f(p.x, p.y);
                }
            }
        }
        // SAFETY: preserve the original request's error state.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn clip(rect: *const RECT) -> i32 {
    boundary(0, || {
        let f = original!(CLIP, unsafe extern "system" fn(*const RECT) -> i32);
        // SAFETY: forwards caller pointer to original validation.
        let result = unsafe { f(rect) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0 && captured() {
            let r = if rect.is_null() {
                None
            } else {
                // SAFETY: successful original read this rectangle.
                let r = unsafe { *rect };
                Some(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
            };
            access(|l| l.clip(r));
            // SAFETY: release the clip imposed by the just-validated request while overlay owns pointer.
            unsafe {
                f(std::ptr::null());
            }
        }
        // SAFETY: preserve original error state.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn get_clip(out: *mut RECT) -> i32 {
    boundary(0, || {
        let f = original!(GET_CLIP, unsafe extern "system" fn(*mut RECT) -> i32);
        // SAFETY: forwards caller pointer, validates before overwrite.
        let result = unsafe { f(out) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0
            && captured()
            && let Some(Some(r)) = access(|l| l.intended.clip)
        {
            // SAFETY: successful original wrote this output rectangle.
            unsafe { *out = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom } }
        }
        // SAFETY: preserve original error state.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn show(on: i32) -> i32 {
    boundary(0, || {
        let f = original!(SHOW, unsafe extern "system" fn(i32) -> i32);
        if captured()
            && owner_queue()
            && let Some(count) = access(|l| l.show(on != 0))
        {
            return count;
        }
        // SAFETY: unchanged scalar argument when passthrough or capture revoked.
        unsafe { f(on) }
    })
}
unsafe extern "system" fn set_shape(shape: HCURSOR) -> HCURSOR {
    boundary(std::ptr::null_mut(), || {
        let f = original!(SET_SHAPE, unsafe extern "system" fn(HCURSOR) -> HCURSOR);
        if captured()
            && owner_queue()
            && let Some(old) = access(|l| l.shape(shape as usize))
        {
            return old as HCURSOR;
        }
        // SAFETY: forwards caller cursor handle to the original.
        unsafe { f(shape) }
    })
}
unsafe extern "system" fn get_shape() -> HCURSOR {
    boundary(std::ptr::null_mut(), || {
        let f = original!(GET_SHAPE, unsafe extern "system" fn() -> HCURSOR);
        if captured()
            && owner_queue()
            && let Some(shape) = access(|l| l.intended.shape)
        {
            return shape as HCURSOR;
        }
        // SAFETY: original takes no arguments.
        unsafe { f() }
    })
}
