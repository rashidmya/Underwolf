use uw_input::cursor::*;
fn snapshot() -> CursorSnapshot {
    CursorSnapshot {
        position: Point { x: -50, y: 20 },
        clip: Some(Rect { left: -100, top: 0, right: 100, bottom: 100 }),
        shape: 9,
        display_count: -2,
    }
}
#[test]
fn focus_loss_never_reclips_or_warps() {
    let mut l = CursorLedger::new(snapshot(), 2);
    let actions = l.release(false, true);
    assert!(!actions.iter().any(|a| matches!(a, CursorAction::Warp(_) | CursorAction::Clip(Some(_)))));
    assert!(actions.contains(&CursorAction::Clip(None)));
}
#[test]
fn game_changes_survive_capture() {
    let mut l = CursorLedger::new(snapshot(), 2);
    l.shape(11);
    assert_eq!(l.show(false), -3);
    l.clip(None);
    let actions = l.release(true, true);
    assert!(actions.contains(&CursorAction::Shape(11)));
    assert!(actions.contains(&CursorAction::Balance(-3)));
    assert!(actions.contains(&CursorAction::Clip(None)));
}
#[test]
fn destroyed_window_drops_stale_state() {
    let mut l = CursorLedger::new(snapshot(), 2);
    assert!(
        !l.release(true, false)
            .iter()
            .any(|a| matches!(a, CursorAction::Warp(_) | CursorAction::Clip(Some(_)) | CursorAction::Shape(_)))
    );
}
#[test]
fn release_is_idempotent() {
    let mut l = CursorLedger::new(snapshot(), 2);
    assert!(!l.release(true, true).is_empty());
    assert!(l.release(true, true).is_empty());
}
#[test]
fn failed_restoration_retries_without_rebalancing_completed_steps() {
    let mut ledger = CursorLedger::new(snapshot(), 2);
    let mut restore = RestorePlan::new(ledger.release(true, true));
    assert_eq!(restore.next(true, true), Some(CursorAction::Clip(None)));
    restore.complete();
    assert_eq!(restore.next(true, true), Some(CursorAction::Balance(-2)));
    restore.complete();
    let failed = restore.next(true, true);
    assert!(matches!(failed, Some(CursorAction::Warp(_))));
    assert_eq!(restore.next(true, true), failed);
    // Focus lost while the failed step was waiting: stale warp/clip/shape must disappear.
    assert_eq!(restore.next(false, true), None);
}
