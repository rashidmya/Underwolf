//! The data thread's reads on each pass (design 2b flow step 5), apart from the Win32 wait, so the
//! rule is unit-tested on Linux: a bell reads its own region and a timeout reads both, and a read
//! that a busy lock skipped stays owed until it succeeds, on the next pass whatever woke it.

/// What woke the data thread's wait, other than `disconnect` (which ends the relay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    InfoBell,
    EventsBell,
    Timeout,
}

/// The reads a busy lock skipped, still owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Owed {
    pub info: bool,
    pub events: bool,
}

impl Owed {
    /// The reads for a pass that `woke`, as `(info, events)`: the bell's own region (both on a
    /// timeout), and any read owed from an earlier pass.
    pub fn reads(&self, woke: Woke) -> (bool, bool) {
        let info = self.info || matches!(woke, Woke::InfoBell | Woke::Timeout);
        let events = self.events || matches!(woke, Woke::EventsBell | Woke::Timeout);
        (info, events)
    }
}
