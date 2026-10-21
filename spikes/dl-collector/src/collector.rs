//! The collector: resolves roots and fields once, then runs the tasks on every `tick` and
//! publishes what they staged through a `ChannelOut`.
//!
//! Publishing rules: one `ChannelOut::commit` per pass, holding only the keys whose value
//! differs from what was last committed (a pass that changes nothing does not commit at all);
//! a key that is unknown to the schema or whose value exceeds its maximum length is dropped.
//! Events go through one FIFO, so a throttled event is retried first on the next tick.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};

use uw_spike_gep_channel::{Schema, Throttle};

use crate::keys::{GEP_INTERNAL, PLUGIN_ERROR_MAX, deadlock_schema};
use crate::layout::broken_tasks;
use crate::mem::MemRead;
use crate::module::Module;
use crate::roots::{resolve_clock_roots, resolve_main_loop, resolve_roots};
use crate::runner::{Pass, Runner};
use crate::schema::SchemaFields;
use crate::tasks::{Built, build, json_str};
use crate::timing::CallerFilter;

/// How far `corrupt=` moves a field offset.
const CORRUPT_DELTA: u32 = 8;
/// Overwolf allows 30 events per 200 ms.
const EVENT_BURST: usize = 30;
const EVENT_WINDOW_DIV: u64 = 5;
/// Events kept while the channel refuses them; the oldest are dropped beyond this.
const EVENT_QUEUE_MAX: usize = 256;
const STATS_INTERVAL_S: u64 = 10;
/// No pass for this long while scanning publishes `plugin_status="stalled"`.
pub const STALL_S: u64 = 5;
const STATS_WINDOW_MAX: usize = 1024;
/// Longest reason text inside `disabled_tasks`.
const REASON_MAX: usize = 100;

pub trait ChannelOut {
    /// Applies all `sets` as one transaction (`None` clears a key) and signals the consumer once.
    fn commit(&mut self, sets: &[(String, String, Option<Vec<u8>>)], ts: u64);
    /// `false`: the event was not accepted (throttled); the collector offers it again later.
    fn push_event(&mut self, name: &str, data: &[u8], ts: u64) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Shutdown,
    Initializing,
    Initialized,
    Scanning,
    /// Scanning, but no pass ran for `STALL_S` (the main-loop call site stopped firing).
    Stalled,
    /// Started with `selftest=1`: nothing is resolved or read.
    Selftest,
    Error(String),
}

impl Status {
    fn text(&self) -> &'static str {
        match self {
            Status::Shutdown => "shutdown",
            Status::Initializing => "initializing",
            Status::Initialized => "initialized",
            Status::Scanning => "scanning",
            Status::Stalled => "stalled",
            Status::Selftest => "selftest",
            Status::Error(_) => "error",
        }
    }
}

#[derive(Default)]
struct Args {
    selftest: bool,
    corrupt: Vec<(String, String)>,
}

impl Args {
    /// Space-separated `key=value`; tokens without `=` and unknown keys (`discover=` is the
    /// plugin's) are ignored.
    fn parse(s: &str) -> Args {
        let mut a = Args::default();
        for (key, value) in s.split_whitespace().filter_map(|t| t.split_once('=')) {
            match key {
                "selftest" => a.selftest = value == "1",
                "corrupt" => {
                    if let Some((class, field)) = value.split_once('.')
                        && !class.is_empty()
                        && !field.is_empty()
                    {
                        a.corrupt.push((class.into(), field.into()));
                    }
                }
                _ => {}
            }
        }
        a
    }
}

pub struct Collector {
    status: Status,
    runner: Runner,
    out: Box<dyn ChannelOut>,
    clock: Box<dyn Fn() -> u64 + Send>,
    freq: u64,
    max_len: HashMap<(String, String), u32>,
    throttle: Throttle,
    queue: VecDeque<(String, String)>,
    committed: BTreeMap<(String, String), Vec<u8>>,
    unresolved: Vec<(&'static str, String)>,
    durations: Vec<u64>,
    window_passes: u64,
    last_stats: Option<u64>,
    /// When the last pass ended (or scanning began).
    last_pass: u64,
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// What `Collector::resolve` found.
struct Resolved {
    built: Built,
    /// The schema layout name.
    layout: &'static str,
    /// Tasks the layout check switches off before they ever run, with the invalid reason.
    broken: Vec<(&'static str, String)>,
}

fn key_limits(schema: &Schema) -> HashMap<(String, String), u32> {
    schema
        .keys()
        .map(|(c, k, max)| ((c.to_string(), k.to_string()), max))
        .collect()
}

impl Collector {
    /// Resolves the roots and fields (or publishes the failure), switches off every task with a
    /// field that fails the layout check (`crate::layout`), and publishes the first status.
    /// `clock` counts ticks of `freq` per second. Arguments: see `Args::parse`.
    pub fn start(
        m: &dyn MemRead,
        client: &Module,
        out: Box<dyn ChannelOut>,
        args: &str,
        clock: Box<dyn Fn() -> u64 + Send>,
        freq: u64,
    ) -> Collector {
        let freq = freq.max(1);
        let mut c = Collector {
            status: Status::Initializing,
            runner: Runner::new(Vec::new()),
            out,
            clock,
            freq,
            max_len: key_limits(&deadlock_schema()),
            throttle: Throttle::new((freq / EVENT_WINDOW_DIV).max(1), EVENT_BURST),
            queue: VecDeque::new(),
            committed: BTreeMap::new(),
            unresolved: Vec::new(),
            durations: Vec::new(),
            window_passes: 0,
            last_stats: None,
            last_pass: 0,
        };
        let args = Args::parse(args);
        let result = catch_unwind(AssertUnwindSafe(|| {
            if args.selftest {
                c.begin_selftest();
            } else {
                match Collector::resolve(m, client, &args) {
                    Ok(resolved) => c.begin(resolved),
                    Err(why) => c.fail(&why),
                }
            }
        }));
        if result.is_err() {
            c.fail("collector panicked at start");
        }
        c
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// One pass over the tasks. Never panics; a failure of the collector itself ends in
    /// `Status::Error` and later ticks do nothing.
    pub fn tick(&mut self, m: &dyn MemRead) {
        if catch_unwind(AssertUnwindSafe(|| self.tick_inner(m))).is_err() {
            self.fail("collector panicked");
        }
    }

    /// The main-loop hook is armed: `Initialized` becomes `Scanning` (published). Any other state
    /// is left alone.
    pub fn scanning(&mut self) {
        if self.status != Status::Initialized {
            return;
        }
        self.status = Status::Scanning;
        self.last_pass = (self.clock)();
        let mut pass = Pass::default();
        pass.set(
            GEP_INTERNAL,
            "plugin_status",
            Some(self.status.text().into()),
        );
        self.publish(&pass);
    }

    /// Called periodically off the main thread: while scanning, no pass for `STALL_S` publishes
    /// `plugin_status="stalled"`. The next pass publishes `"scanning"` again.
    pub fn check_stall(&mut self) {
        if self.status != Status::Scanning {
            return;
        }
        let now = (self.clock)();
        if u128::from(now.saturating_sub(self.last_pass))
            < u128::from(STALL_S) * u128::from(self.freq)
        {
            return;
        }
        self.status = Status::Stalled;
        let mut pass = Pass::default();
        pass.set(
            GEP_INTERNAL,
            "plugin_status",
            Some(self.status.text().into()),
        );
        let _ = catch_unwind(AssertUnwindSafe(|| self.publish(&pass)));
    }

    /// Publishes `plugin_status="shutdown"`; later ticks do nothing.
    pub fn shutdown(&mut self) {
        self.status = Status::Shutdown;
        let mut pass = Pass::default();
        pass.set(GEP_INTERNAL, "plugin_status", Some("shutdown".into()));
        self.publish(&pass);
    }

    fn resolve(m: &dyn MemRead, client: &Module, args: &Args) -> Result<Resolved, String> {
        let roots = resolve_roots(m, client).map_err(|e| e.to_string())?;
        let mut fields = SchemaFields::load(m, client).map_err(|e| format!("schema: {e}"))?;
        // Moves a resolved offset on purpose (the fail-safe run); an unresolved name is ignored.
        for (class, field) in &args.corrupt {
            fields.add_offset(class, field, CORRUPT_DELTA);
        }
        let built = build(&roots, resolve_clock_roots(m, client), &fields)?;
        // After `corrupt=`: a moved field fails its relation like a real layout change.
        Ok(Resolved {
            built,
            layout: fields.layout_name(),
            broken: broken_tasks(&fields),
        })
    }

    fn begin(&mut self, resolved: Resolved) {
        let Resolved {
            built,
            layout,
            broken,
        } = resolved;
        self.runner = Runner::new(built.tasks);
        for (task, why) in &broken {
            // A task that was not built is already listed as unresolved.
            self.runner.disable(task, why);
        }
        self.unresolved = built.unresolved;
        self.status = Status::Initialized;
        let mut pass = Pass::default();
        let version = format!(
            "{{\"collector\":\"uw-spike-dlcollector {}\",\"schema_layout\":{}}}",
            env!("CARGO_PKG_VERSION"),
            json_str(layout)
        );
        pass.set(GEP_INTERNAL, "version_info", Some(version));
        pass.set(
            GEP_INTERNAL,
            "plugin_status",
            Some(self.status.text().into()),
        );
        pass.set(GEP_INTERNAL, "disabled_tasks", Some(self.disabled_json()));
        self.publish(&pass);
    }

    fn begin_selftest(&mut self) {
        self.status = Status::Selftest;
        let mut pass = Pass::default();
        pass.set(
            GEP_INTERNAL,
            "plugin_status",
            Some(self.status.text().into()),
        );
        self.publish(&pass);
        self.enqueue("selftest", "{}".into());
        self.flush_events();
    }

    /// Publishes `plugin_status="error"` and the reason, and clears every game value that was
    /// committed before (nothing stays visible that the collector can no longer vouch for).
    /// Later ticks do nothing. Also used when the main-loop caller is not found
    /// (`main_loop_filter`).
    pub fn fail(&mut self, why: &str) {
        self.status = Status::Error(why.to_string());
        let mut pass = Pass::default();
        for (cat, key) in self.committed.keys() {
            if cat != GEP_INTERNAL {
                pass.set(cat, key, None);
            }
        }
        pass.set(GEP_INTERNAL, "plugin_status", Some("error".into()));
        pass.set(
            GEP_INTERNAL,
            "plugin_error",
            Some(truncate(why, PLUGIN_ERROR_MAX as usize).into()),
        );
        // The channel itself may be what failed.
        let _ = catch_unwind(AssertUnwindSafe(|| self.publish(&pass)));
    }

    /// The main-loop caller in the loaded `modules` (the plugin's start path). Not resolved (not
    /// found, ambiguous or unreadable): the collector fails with the reason (see `fail`) and
    /// there is no filter, so no pass is ever armed.
    pub fn main_loop_filter(
        &mut self,
        m: &dyn MemRead,
        modules: &[Module],
    ) -> Option<CallerFilter> {
        match resolve_main_loop(m, modules) {
            Ok(filter) => Some(filter),
            Err(why) => {
                self.fail(&why);
                None
            }
        }
    }

    fn tick_inner(&mut self, m: &dyn MemRead) {
        match self.status {
            Status::Error(_) | Status::Shutdown | Status::Initializing => return,
            Status::Selftest => {
                self.flush_events();
                return;
            }
            Status::Initialized | Status::Scanning | Status::Stalled => {}
        }
        let started = (self.clock)();
        let mut pass = self.runner.pass(m, &*self.clock, self.freq);
        let now = (self.clock)();
        self.status = Status::Scanning;
        pass.set(
            GEP_INTERNAL,
            "plugin_status",
            Some(self.status.text().into()),
        );
        pass.set(GEP_INTERNAL, "disabled_tasks", Some(self.disabled_json()));
        self.last_pass = now;
        self.record_duration(now.saturating_sub(started));
        if let Some(stats) = self.stats_due(now) {
            pass.set(GEP_INTERNAL, "pass_stats", Some(stats));
        }
        self.publish(&pass);
        for (name, data) in pass.events() {
            self.enqueue(name, data.clone());
        }
        self.flush_events();
    }

    fn disabled_json(&self) -> String {
        let runtime = self.runner.disabled();
        let entries: Vec<String> = self
            .unresolved
            .iter()
            .map(|(task, why)| (*task, why))
            .chain(runtime.iter().map(|(task, why)| (*task, why)))
            .map(|(task, why)| {
                format!(
                    "{{\"task\":{},\"reason\":{}}}",
                    json_str(task),
                    json_str(truncate(why, REASON_MAX))
                )
            })
            .collect();
        format!("[{}]", entries.join(","))
    }

    fn record_duration(&mut self, ticks: u64) {
        self.window_passes += 1;
        if self.durations.len() == STATS_WINDOW_MAX {
            self.durations.remove(0);
        }
        self.durations.push(ticks);
    }

    /// The `pass_stats` JSON once every `STATS_INTERVAL_S`, then the window starts over.
    fn stats_due(&mut self, now: u64) -> Option<String> {
        let Some(last) = self.last_stats else {
            self.last_stats = Some(now);
            return None;
        };
        let interval = u128::from(STATS_INTERVAL_S) * u128::from(self.freq);
        if u128::from(now.saturating_sub(last)) < interval {
            return None;
        }
        let mut sorted = std::mem::take(&mut self.durations);
        sorted.sort_unstable();
        let ms = |ticks: u64| ticks as f64 * 1000.0 / self.freq as f64;
        let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
        let max = sorted.last().copied().unwrap_or(0);
        let json = format!(
            "{{\"median_ms\":{:.3},\"max_ms\":{:.3},\"passes\":{}}}",
            ms(median),
            ms(max),
            self.window_passes
        );
        self.window_passes = 0;
        self.last_stats = Some(now);
        Some(json)
    }

    fn timestamp(&self) -> u64 {
        let ms = u128::from((self.clock)()) * 1000 / u128::from(self.freq);
        u64::try_from(ms).unwrap_or(u64::MAX)
    }

    /// Commits the changed, valid keys of `pass` in one transaction.
    fn publish(&mut self, pass: &Pass) {
        let mut sets = Vec::new();
        for (cat, key, value) in pass.sets() {
            let id = (cat.clone(), key.clone());
            let Some(&max) = self.max_len.get(&id) else {
                continue;
            };
            let bytes = value.as_ref().map(|v| v.as_bytes().to_vec());
            if bytes.as_ref().is_some_and(|b| b.len() > max as usize) {
                continue;
            }
            if self.committed.get(&id) == bytes.as_ref() {
                continue;
            }
            sets.push((id.0, id.1, bytes));
        }
        if sets.is_empty() {
            return;
        }
        let ts = self.timestamp();
        self.out.commit(&sets, ts);
        for (cat, key, bytes) in sets {
            match bytes {
                Some(b) => self.committed.insert((cat, key), b),
                None => self.committed.remove(&(cat, key)),
            };
        }
    }

    fn enqueue(&mut self, name: &str, data: String) {
        if self.queue.len() == EVENT_QUEUE_MAX {
            self.queue.pop_front();
        }
        self.queue.push_back((name.into(), data));
    }

    /// Offers queued events oldest first until the throttle or the channel refuses one.
    fn flush_events(&mut self) {
        while let Some((name, data)) = self.queue.front() {
            let now = (self.clock)();
            if !self.throttle.admit(now) {
                break;
            }
            let ts =
                u64::try_from(u128::from(now) * 1000 / u128::from(self.freq)).unwrap_or(u64::MAX);
            if !self.out.push_event(name, data.as_bytes(), ts) {
                break;
            }
            self.queue.pop_front();
        }
    }
}
