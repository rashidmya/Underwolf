//! Requests the relay sent and is waiting on: `load_overlay`s awaiting their `status` (design 2a
//! "Plan 1 items", late reply) and `ping`s awaiting their `pong` (owner decision D1). Each has its
//! own deadline; a reply whose id is not pending (a late reply to a request that already timed
//! out, or a stray one) is discarded, so every command gets exactly one reply.

#[derive(Debug, Default)]
pub struct Pending {
    entries: Vec<(u32, u64)>,
}

impl Pending {
    pub fn new() -> Pending {
        Pending::default()
    }

    /// `id` was sent; it times out at `deadline_ms`.
    pub fn add(&mut self, id: u32, deadline_ms: u64) {
        self.entries.push((id, deadline_ms));
    }

    /// A `status` for `id` arrived: `true` (and forgotten) if it was pending, `false` to discard it.
    pub fn take(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(p, _)| *p != id);
        self.entries.len() != before
    }

    /// The requests past their deadline at `now_ms`, forgotten (each gets `status_timeout`).
    pub fn expire(&mut self, now_ms: u64) -> Vec<u32> {
        let (late, keep): (Vec<_>, Vec<_>) = self.entries.iter().partition(|(_, d)| now_ms >= *d);
        self.entries = keep;
        late.into_iter().map(|(id, _)| id).collect()
    }

    /// Every pending request, forgotten (the bootstrap went away).
    pub fn drain(&mut self) -> Vec<u32> {
        self.entries.drain(..).map(|(id, _)| id).collect()
    }

    /// The earliest deadline, for the relay's wait.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|(_, d)| *d).min()
    }
}
