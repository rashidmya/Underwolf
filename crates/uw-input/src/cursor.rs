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
