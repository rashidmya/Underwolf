//! Sliding-window send throttle (Overwolf allows 30 events per 200 ms).

use std::collections::VecDeque;

pub struct Throttle {
    window_ticks: u64,
    max: usize,
    sent: VecDeque<u64>,
}

impl Throttle {
    pub fn new(window_ticks: u64, max: usize) -> Throttle {
        Throttle { window_ticks, max, sent: VecDeque::with_capacity(max) }
    }

    /// Returns true and records `now` if fewer than `max` events were admitted in the last window.
    pub fn admit(&mut self, now: u64) -> bool {
        while let Some(&t) = self.sent.front() {
            if now.saturating_sub(t) >= self.window_ticks {
                self.sent.pop_front();
            } else {
                break;
            }
        }
        if self.sent.len() >= self.max {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}
