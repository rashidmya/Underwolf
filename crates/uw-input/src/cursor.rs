//! Portable cursor restoration intent, separate from temporary overlay changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[derive(Debug, Clone, Copy)]
pub struct CursorSnapshot {
    pub position: Point,
    pub clip: Option<Rect>,
    pub shape: usize,
    pub display_count: i32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAction {
    Warp(Point),
    Clip(Option<Rect>),
    Shape(usize),
    Balance(i32),
}
pub struct CursorLedger {
    pub frozen: Point,
    pub intended: CursorSnapshot,
    baseline: i32,
    adjustment: i32,
    released: bool,
}
impl CursorLedger {
    pub fn new(snapshot: CursorSnapshot, adjustment: i32) -> Self {
        Self {
            frozen: snapshot.position,
            intended: snapshot,
            baseline: snapshot.display_count,
            adjustment,
            released: false,
        }
    }
    pub fn position(&mut self, p: Point) {
        self.intended.position = p
    }
    pub fn clip(&mut self, r: Option<Rect>) {
        self.intended.clip = r
    }
    pub fn shape(&mut self, s: usize) -> usize {
        let old = self.intended.shape;
        self.intended.shape = s;
        old
    }
    pub fn show(&mut self, on: bool) -> i32 {
        self.intended.display_count = self.intended.display_count.saturating_add(if on { 1 } else { -1 });
        self.intended.display_count
    }
    pub fn release(&mut self, foreground: bool, window_alive: bool) -> Vec<CursorAction> {
        if self.released {
            return vec![];
        }
        self.released = true;
        let balance = self.intended.display_count.saturating_sub(self.baseline).saturating_sub(self.adjustment);
        let mut actions = vec![CursorAction::Clip(None), CursorAction::Balance(balance)];
        if foreground && window_alive {
            actions.extend([
                CursorAction::Warp(self.intended.position),
                CursorAction::Clip(self.intended.clip),
                CursorAction::Shape(self.intended.shape),
            ])
        }
        actions
    }
}

/// A failed OS operation stays pending. Completed count adjustments are never replayed.
pub struct RestorePlan(std::collections::VecDeque<CursorAction>);
impl RestorePlan {
    pub fn new(actions: Vec<CursorAction>) -> Self {
        Self(actions.into())
    }
    pub fn retry(&mut self, foreground: bool, alive: bool) {
        if (!foreground || !alive) && !self.0.contains(&CursorAction::Clip(None)) {
            self.0.push_front(CursorAction::Clip(None));
        }
    }
    pub fn next(&mut self, foreground: bool, alive: bool) -> Option<CursorAction> {
        if !foreground || !alive {
            self.0.retain(|a| matches!(a, CursorAction::Clip(None) | CursorAction::Balance(_)));
        }
        self.0.front().copied().map(|a| match a {
            CursorAction::Balance(n) => CursorAction::Balance(n.clamp(-4096, 4096)),
            other => other,
        })
    }
    pub fn complete(&mut self) {
        if let Some(CursorAction::Balance(n)) = self.0.front_mut() {
            *n -= (*n).clamp(-4096, 4096);
            if *n != 0 {
                return;
            }
        }
        self.0.pop_front();
    }
}

use portable_atomic::AtomicU128;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
const RESTORING: u64 = 1 << 63;
const MUTATION: u64 = 1 << 20;
/// Successful cursor writes remain available while an owner restore is pending.
/// The install gate requires hardware atomics; the hook never uses the library's lock fallback.
pub struct CursorJournal {
    state: AtomicU64,
    versions: [AtomicU64; 3],
    position: AtomicU64,
    clip: AtomicU128,
    shape: AtomicUsize,
}
impl Default for CursorJournal {
    fn default() -> Self {
        Self::new()
    }
}
impl CursorJournal {
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
            versions: [const { AtomicU64::new(0) }; 3],
            position: AtomicU64::new(0),
            clip: AtomicU128::new(1),
            shape: AtomicUsize::new(0),
        }
    }
    pub fn lock_free() -> bool {
        AtomicU128::is_lock_free()
    }
    pub fn checkpoint(&self) -> [u64; 3] {
        self.versions.each_ref().map(|v| v.load(Ordering::Acquire))
    }
    pub fn begin_call(&self) -> CursorCall<'_> {
        let before = self.state.fetch_add(MUTATION + 1, Ordering::AcqRel);
        CursorCall(self, before & RESTORING != 0)
    }
    pub fn begin_restore(&self) -> Option<u64> {
        let state = self.state.load(Ordering::Acquire);
        if state & (RESTORING | (MUTATION - 1)) != 0 {
            return None;
        }
        self.state
            .compare_exchange(state, state | RESTORING, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| state | RESTORING)
    }
    pub fn commit_restore(&self, token: u64) -> bool {
        if self.state.compare_exchange(token, token & !RESTORING, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            true
        } else {
            self.cancel_restore();
            false
        }
    }
    pub fn cancel_restore(&self) {
        self.state.fetch_and(!RESTORING, Ordering::Release);
    }
    pub fn merge(&self, plan: &mut RestorePlan, baseline: [u64; 3]) {
        let versions = self.checkpoint();
        if versions[0] != baseline[0] {
            let value = self.position.load(Ordering::Acquire);
            plan.replace(CursorAction::Warp(Point { x: value as u32 as i32, y: (value >> 32) as u32 as i32 }));
        }
        if versions[1] != baseline[1] {
            let value = self.clip.load(Ordering::Acquire);
            // Sentinel is an inverted rectangle (left=1,right=0), never a successful clip request.
            let rect = (value != 1).then_some(Rect {
                left: value as u32 as i32,
                top: (value >> 32) as u32 as i32,
                right: (value >> 64) as u32 as i32,
                bottom: (value >> 96) as u32 as i32,
            });
            plan.replace(CursorAction::Clip(rect));
        }
        if versions[2] != baseline[2] {
            plan.replace(CursorAction::Shape(self.shape.load(Ordering::Acquire)));
        }
    }
}
pub struct CursorCall<'a>(&'a CursorJournal, bool);
impl CursorCall<'_> {
    pub fn interfered(&self) -> bool {
        self.1
    }
    pub fn position(&self, p: Point) {
        self.0.position.store(p.x as u32 as u64 | ((p.y as u32 as u64) << 32), Ordering::Release);
        self.0.versions[0].fetch_add(1, Ordering::Release);
    }
    pub fn clip(&self, r: Option<Rect>) {
        let value = r.map_or(1, |r| {
            r.left as u32 as u128
                | ((r.top as u32 as u128) << 32)
                | ((r.right as u32 as u128) << 64)
                | ((r.bottom as u32 as u128) << 96)
        });
        self.0.clip.store(value, Ordering::Release);
        self.0.versions[1].fetch_add(1, Ordering::Release);
    }
    pub fn shape(&self, shape: usize) {
        self.0.shape.store(shape, Ordering::Release);
        self.0.versions[2].fetch_add(1, Ordering::Release);
    }
}
impl Drop for CursorCall<'_> {
    fn drop(&mut self) {
        self.0.state.fetch_sub(1, Ordering::Release);
    }
}
impl RestorePlan {
    fn replace(&mut self, action: CursorAction) {
        self.0.retain(|old| {
            matches!((old, action), (CursorAction::Clip(None), CursorAction::Clip(_)))
                || std::mem::discriminant(old) != std::mem::discriminant(&action)
        });
        if !self.0.contains(&action) {
            self.0.push_back(action);
        }
    }
}
