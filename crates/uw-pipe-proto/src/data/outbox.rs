//! The producer's pending work (design 2b "Producer API"), without the Win32 objects, so its
//! rules are unit-tested on Linux. `uw_game_channel::data::Producer` wraps it.
//!
//! - `commit` checks every entry first (an unknown key or a value over its `max_len` is an error,
//!   and then nothing is kept), then keeps the newest value per key, each with its own timestamp,
//!   until `write_info` runs under the info lock.
//! - `push_event` checks the name and the data, then queues the event in a FIFO of `QUEUE_CAP`;
//!   past that the oldest is dropped and counted.
//! - `write_events` writes queued events into the ring under the events lock, as many as one
//!   throttle of `THROTTLE_MAX` per `THROTTLE_WINDOW_MS` admits (the rest wait in the queue), and
//!   the dropped count with them. A busy lock spends nothing: the throttle is asked only once the
//!   lock is held.

use std::collections::{BTreeMap, VecDeque};

use super::{
    InfoLayout, MAX_EVENT_DATA, MAX_EVENT_NAME, Schema, info_set, ring_push, ring_set_dropped,
};

pub const QUEUE_CAP: usize = 256;
pub const THROTTLE_MAX: usize = 30;
pub const THROTTLE_WINDOW_MS: u64 = 200;

/// Why the producer refused a call. Nothing is silently dropped or truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProduceError {
    UnknownKey {
        category: String,
        key: String,
    },
    ValueTooLong {
        category: String,
        key: String,
        len: usize,
        max_len: u32,
    },
    NameTooLong(usize),
    DataTooLong(usize),
}

impl std::fmt::Display for ProduceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProduceError::UnknownKey { category, key } => {
                write!(f, "{category}.{key} is not in the schema")
            }
            ProduceError::ValueTooLong {
                category,
                key,
                len,
                max_len,
            } => {
                write!(
                    f,
                    "{category}.{key}: a value of {len} bytes, over its max_len {max_len}"
                )
            }
            ProduceError::NameTooLong(n) => {
                write!(f, "an event name of {n} bytes, over {MAX_EVENT_NAME}")
            }
            ProduceError::DataTooLong(n) => {
                write!(f, "event data of {n} bytes, over {MAX_EVENT_DATA}")
            }
        }
    }
}
impl std::error::Error for ProduceError {}

/// A sliding-window throttle: at most `max` admitted in any `window_ms`.
#[derive(Debug, Clone)]
pub struct Throttle {
    window_ms: u64,
    max: usize,
    admitted: VecDeque<u64>,
}

impl Throttle {
    pub fn new(window_ms: u64, max: usize) -> Throttle {
        Throttle {
            window_ms,
            max,
            admitted: VecDeque::with_capacity(max),
        }
    }

    /// Admit one more at `now` (ms) if fewer than `max` were admitted in the last `window_ms`.
    pub fn admit(&mut self, now: u64) -> bool {
        while self
            .admitted
            .front()
            .is_some_and(|&t| now.saturating_sub(t) >= self.window_ms)
        {
            self.admitted.pop_front();
        }
        if self.admitted.len() >= self.max {
            return false;
        }
        self.admitted.push_back(now);
        true
    }
}

#[derive(Debug, Clone)]
struct Queued {
    name: String,
    data: String,
    ts: u64,
}

/// The producer's pending info and queued events.
#[derive(Debug, Clone)]
pub struct Outbox {
    schema: Schema,
    layout: InfoLayout,
    /// Key index -> the newest value committed and its timestamp.
    pending: BTreeMap<usize, (Option<String>, u64)>,
    queue: VecDeque<Queued>,
    dropped: u64,
    /// The dropped count last written into the ring.
    ring_dropped: u64,
    throttle: Throttle,
}

impl Outbox {
    /// The schema is parsed once, here (the `Producer` is created with it).
    pub fn new(schema: Schema) -> Outbox {
        Outbox {
            layout: InfoLayout::of(&schema),
            schema,
            pending: BTreeMap::new(),
            queue: VecDeque::new(),
            dropped: 0,
            ring_dropped: 0,
            throttle: Throttle::new(THROTTLE_WINDOW_MS, THROTTLE_MAX),
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn info_layout(&self) -> &InfoLayout {
        &self.layout
    }

    /// One info transaction at time `ts`: `(category, key, value)` entries, `None` for null.
    pub fn commit(
        &mut self,
        entries: &[(&str, &str, Option<&str>)],
        ts: u64,
    ) -> Result<(), ProduceError> {
        let mut checked = Vec::with_capacity(entries.len());
        for &(category, key, value) in entries {
            let Some(i) = self.schema.key_index(category, key) else {
                return Err(ProduceError::UnknownKey {
                    category: category.into(),
                    key: key.into(),
                });
            };
            let max_len = self.layout.max_len(i);
            if let Some(v) = value
                && v.len() > max_len as usize
            {
                return Err(ProduceError::ValueTooLong {
                    category: category.into(),
                    key: key.into(),
                    len: v.len(),
                    max_len,
                });
            }
            checked.push((i, value));
        }
        for (i, value) in checked {
            self.pending.insert(i, (value.map(str::to_string), ts));
        }
        Ok(())
    }

    /// Whether `write_info` has anything to write.
    pub fn has_pending_info(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Under the info lock: write every pending value with its own timestamp. Returns whether
    /// any key changed (then the producer rings `info-bell` once).
    pub fn write_info(&mut self, region: &mut [u8]) -> bool {
        let mut changed = false;
        for (i, (value, ts)) in std::mem::take(&mut self.pending) {
            changed |= info_set(region, &self.layout, i, value.as_deref(), ts);
        }
        changed
    }

    /// Queue an event pushed at time `ts`. A full queue drops its oldest event and counts it.
    pub fn push_event(&mut self, name: &str, data: &str, ts: u64) -> Result<(), ProduceError> {
        if name.len() > MAX_EVENT_NAME {
            return Err(ProduceError::NameTooLong(name.len()));
        }
        if data.len() > MAX_EVENT_DATA {
            return Err(ProduceError::DataTooLong(data.len()));
        }
        if self.queue.len() == QUEUE_CAP {
            self.queue.pop_front();
            self.dropped += 1;
        }
        self.queue.push_back(Queued {
            name: name.into(),
            data: data.into(),
            ts,
        });
        Ok(())
    }

    /// Events waiting for the ring.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Events the queue dropped so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether `write_events` has anything to write (events, or a new dropped count).
    pub fn has_pending_events(&self) -> bool {
        !self.queue.is_empty() || self.dropped != self.ring_dropped
    }

    /// Under the events lock, at time `now`: write queued events in order while the throttle
    /// admits them, then the dropped count. Returns whether the ring changed (then the producer
    /// rings `events-bell` once).
    pub fn write_events(&mut self, region: &mut [u8], now: u64) -> bool {
        let mut changed = false;
        while !self.queue.is_empty() && self.throttle.admit(now) {
            if let Some(e) = self.queue.pop_front() {
                ring_push(region, &e.name, &e.data, e.ts);
                changed = true;
            }
        }
        if self.dropped != self.ring_dropped {
            ring_set_dropped(region, self.dropped);
            self.ring_dropped = self.dropped;
            changed = true;
        }
        changed
    }
}
