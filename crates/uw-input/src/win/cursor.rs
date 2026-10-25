//! Cursor originals and game-visible intent. Restoration runs on the captured GUI thread.
use super::*;
use crate::cursor::{CursorAction, CursorJournal, CursorLedger, CursorSnapshot, Point, Rect, RestorePlan};
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
static JOURNAL: CursorJournal = CursorJournal::new();
static BASELINE: Mutex<[u64; 3]> = Mutex::new([0; 3]);
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
            address: hooks::resolve(c"GetCursorPos"),
            detour: get_pos as *const () as usize,
            original: &GET_POS,
        },
        hooks::Target {
            address: hooks::resolve(c"SetCursorPos"),
            detour: set_pos as *const () as usize,
            original: &SET_POS,
        },
        hooks::Target { address: hooks::resolve(c"ClipCursor"), detour: clip as *const () as usize, original: &CLIP },
        hooks::Target {
            address: hooks::resolve(c"GetClipCursor"),
            detour: get_clip as *const () as usize,
            original: &GET_CLIP,
        },
        hooks::Target { address: hooks::resolve(c"ShowCursor"), detour: show as *const () as usize, original: &SHOW },
        hooks::Target {
            address: hooks::resolve(c"SetCursor"),
            detour: set_shape as *const () as usize,
            original: &SET_SHAPE,
        },
        hooks::Target {
            address: hooks::resolve(c"GetCursor"),
            detour: get_shape as *const () as usize,
            original: &GET_SHAPE,
        },
    ]
}
fn captured() -> bool {
    !bypassed()
        && SHARED.get().is_some_and(|s| {
            s.capture()
                || (s.setting_up.load(Ordering::Acquire) && s.release.load(Ordering::Acquire) == 0
                // SAFETY: setup virtualization still requires the owner foreground window.
                && unsafe { GetForegroundWindow() as usize == s.hwnd() })
        })
}
// ShowCursor and cursor shape belong to an input queue, not the process.
// Attached threads share the focused window and the owner's cursor count.
fn owner_queue() -> bool {
    SHARED.get().is_some_and(|s| {
        // SAFETY: thread-local queries take no pointer arguments.
        unsafe { GetCurrentThreadId() == s.thread() || (s.hwnd() != 0 && GetFocus() as usize == s.hwnd()) }
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
pub(super) fn prepare() -> Option<u64> {
    let token = JOURNAL.begin_restore()?;
    let Ok(mut baseline) = BASELINE.try_lock() else {
        JOURNAL.cancel_restore();
        return None;
    };
    *baseline = JOURNAL.checkpoint();
    let success = enter_inner();
    if success {
        Some(token)
    } else {
        JOURNAL.cancel_restore();
        None
    }
}
pub(super) fn activate(token: u64, success: bool) -> bool {
    if success {
        JOURNAL.commit_restore(token)
    } else {
        JOURNAL.cancel_restore();
        false
    }
}
fn track() -> crate::cursor::CursorCall<'static> {
    let transaction = JOURNAL.begin_call();
    if transaction.interfered()
        && let Some(s) = SHARED.get()
    {
        s.revoke(InputReason::Unavailable);
    }
    transaction
}
fn enter_inner() -> bool {
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
    let Some(token) = JOURNAL.begin_restore() else { return false };
    struct Attempt;
    impl Drop for Attempt {
        fn drop(&mut self) {
            JOURNAL.cancel_restore();
        }
    }
    let _attempt = Attempt;
    let Ok(baseline) = BASELINE.try_lock() else { return false };
    let Ok(mut pending) = RESTORE.try_lock() else { return false };
    if pending.is_none() {
        let Ok(mut slot) = CURSOR.try_lock() else { return false };
        let Some(mut ledger) = slot.take() else { return true };
        *pending = Some(RestorePlan::new(ledger.release(foreground, alive)));
    }
    let plan = pending.as_mut().unwrap();
    plan.retry(foreground, alive);
    JOURNAL.merge(plan, *baseline);
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
    if !JOURNAL.commit_restore(token) {
        return false;
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
        let transaction = track();
        let active = captured();
        let before = if active { real_position() } else { None };
        // SAFETY: scalar API arguments forwarded unchanged.
        let result = unsafe { f(x, y) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0 {
            transaction.position(Point { x, y });
        }
        if result != 0 && active {
            access(|l| l.position(Point { x, y }));
            if let Some(p) = before {
                // SAFETY: restore the real overlay pointer after validating the game's request with original API.
                unsafe {
                    f(p.x, p.y);
                }
            }
        }
        drop(transaction);
        // SAFETY: preserve the original request's error state.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn clip(rect: *const RECT) -> i32 {
    boundary(0, || {
        let f = original!(CLIP, unsafe extern "system" fn(*const RECT) -> i32);
        let transaction = track();
        // SAFETY: forwards caller pointer to original validation.
        let result = unsafe { f(rect) };
        // SAFETY: thread-local error query takes no arguments.
        let error = unsafe { GetLastError() };
        if result != 0 {
            let r = if rect.is_null() {
                None
            } else {
                // SAFETY: successful original read this rectangle.
                let r = unsafe { *rect };
                Some(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
            };
            transaction.clip(r);
            if captured() {
                access(|l| l.clip(r));
                // SAFETY: release the clip imposed by the just-validated request while overlay owns pointer.
                unsafe {
                    f(std::ptr::null());
                }
            }
        }
        drop(transaction);
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
        let _transaction = owner_queue().then(track);
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
        if !owner_queue() {
            // SAFETY: independent input queues retain native cursor ownership.
            return unsafe { f(shape) };
        }
        let transaction = track();
        if captured()
            && let Some(old) = access(|l| l.shape(shape as usize))
        {
            transaction.shape(shape as usize);
            return old as HCURSOR;
        }
        // SAFETY: original scalar cursor handle; return is the previous cursor, including null.
        let old = unsafe { f(shape) };
        // SAFETY: immediate caller-visible native error snapshot.
        let error = unsafe { GetLastError() };
        transaction.shape(shape as usize);
        drop(transaction);
        // SAFETY: preserve native error state through bookkeeping.
        unsafe {
            SetLastError(error);
        }
        old
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

/// The old input queue has terminated; its display counter no longer belongs to a live owner.
/// Global confinement still needs releasing, but no old warp/shape/count is applied to a new queue.
pub(super) fn abandon_queue() -> bool {
    let Ok(mut pending) = RESTORE.try_lock() else { return false };
    let Ok(mut ledger) = CURSOR.try_lock() else { return false };
    let clip = original!(CLIP, unsafe extern "system" fn(*const RECT) -> i32);
    // SAFETY: releasing process-global confinement is valid on this surviving worker thread.
    if unsafe { clip(std::ptr::null()) } == 0 {
        return false;
    }
    *pending = None;
    *ledger = None;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, OnceLock};
    static FAIL: AtomicBool = AtomicBool::new(false);
    static PAUSE: AtomicBool = AtomicBool::new(false);
    static PAUSE_CLIP: AtomicBool = AtomicBool::new(false);
    static BARRIERS: OnceLock<(Arc<Barrier>, Arc<Barrier>)> = OnceLock::new();
    unsafe extern "system" fn controlled_position(x: i32, y: i32) -> i32 {
        if x == 50 && FAIL.swap(false, Ordering::AcqRel) {
            // SAFETY: simulate a native failure with its thread-local error.
            unsafe {
                SetLastError(5);
            }
            return 0;
        }
        if x == 50 && PAUSE.swap(false, Ordering::AcqRel) {
            let (entered, resume) = BARRIERS.get().unwrap();
            entered.wait();
            resume.wait();
        }
        // SAFETY: test trampoline forwards the scalar native position request.
        unsafe { SetCursorPos(x, y) }
    }
    unsafe extern "system" fn controlled_clip(rect: *const RECT) -> i32 {
        // SAFETY: forward valid native clip arguments from the adapter/test owner.
        let result = unsafe { ClipCursor(rect) };
        // SAFETY: preserve native error through barrier synchronization.
        let error = unsafe { GetLastError() };
        if rect.is_null() && PAUSE_CLIP.swap(false, Ordering::AcqRel) {
            let (entered, resume) = BARRIERS.get().unwrap();
            entered.wait();
            resume.wait();
        }
        // SAFETY: restore the native caller's thread-local error.
        unsafe {
            SetLastError(error);
        }
        result
    }
    fn pending_warp() {
        *BASELINE.lock().unwrap() = JOURNAL.checkpoint();
        *RESTORE.lock().unwrap() = Some(RestorePlan::new(vec![CursorAction::Warp(Point { x: 50, y: 60 })]));
    }
    #[test]
    fn native_pending_and_inflight_restore_preserve_new_game_position() {
        assert!(CursorJournal::lock_free());
        for target in targets() {
            target.original.store(target.address, Ordering::Release);
        }
        SET_POS.store(controlled_position as *const () as usize, Ordering::Release);
        BARRIERS.set((Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)))).unwrap();
        pending_warp();
        FAIL.store(true, Ordering::Release);
        assert!(!restore(true, true));
        // SAFETY: scalar game call during pending restoration, through the adapter.
        assert_ne!(unsafe { set_pos(100, 110) }, 0);
        assert!(restore(true, true));
        let actual = real_position().unwrap();
        assert_eq!((actual.x, actual.y), (100, 110));
        pending_warp();
        PAUSE.store(true, Ordering::Release);
        let owner = std::thread::spawn(|| restore(true, true));
        let (entered, resume) = BARRIERS.get().unwrap();
        entered.wait();
        // SAFETY: a successful game request overlaps an already-issued owner restore.
        assert_ne!(unsafe { set_pos(120, 130) }, 0);
        resume.wait();
        assert!(!owner.join().unwrap());
        assert!(restore(true, true));
        let actual = real_position().unwrap();
        assert_eq!((actual.x, actual.y), (120, 130));
        CLIP.store(controlled_clip as *const () as usize, Ordering::Release);
        *BASELINE.lock().unwrap() = JOURNAL.checkpoint();
        *RESTORE.lock().unwrap() = Some(RestorePlan::new(vec![CursorAction::Clip(None)]));
        PAUSE_CLIP.store(true, Ordering::Release);
        let owner = std::thread::spawn(|| restore(false, true));
        entered.wait();
        let r = RECT { left: 70, top: 80, right: 300, bottom: 320 };
        // SAFETY: valid game clip arriving after owner unclip but before transaction commit.
        assert_ne!(unsafe { clip(&r) }, 0);
        resume.wait();
        assert!(!owner.join().unwrap());
        assert!(restore(false, true));
        let mut actual = RECT::default();
        // SAFETY: writable native rectangle for the final confinement query.
        assert_ne!(unsafe { GetClipCursor(&mut actual) }, 0);
        assert_ne!((actual.left, actual.top, actual.right, actual.bottom), (70, 80, 300, 320));
    }
}
