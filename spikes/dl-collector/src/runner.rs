//! Task runner (spec §6.4): times every task, disables the slow, the crashing and the invalid
//! ones, and merges the rest of a pass into one staged update.

use std::collections::BTreeSet;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::mem::{MemRead, ReadError};

/// A run over this long counts as an overrun.
pub const OVERRUN_MS: u64 = 20;
/// The overrun that disables a task is the one after this many.
pub const MAX_OVERRUNS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    /// The target changed while the task was reading it, or its state is not coherent yet.
    Torn,
    Read(ReadError),
    /// A layout mismatch; the task cannot be trusted any more.
    Invalid(String),
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Torn => f.write_str("torn read"),
            TaskError::Read(e) => write!(f, "read failed: {e}"),
            TaskError::Invalid(why) => write!(f, "invalid: {why}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl From<ReadError> for TaskError {
    fn from(e: ReadError) -> TaskError {
        TaskError::Read(e)
    }
}

/// Info values and events staged by one pass. A key set twice keeps the last value.
#[derive(Debug, Default, Clone)]
pub struct Pass {
    sets: Vec<(String, String, Option<String>)>,
    events: Vec<(String, String)>,
}

impl Pass {
    pub fn set(&mut self, cat: &str, key: &str, v: Option<String>) {
        match self.sets.iter_mut().find(|(c, k, _)| c == cat && k == key) {
            Some(slot) => slot.2 = v,
            None => self.sets.push((cat.into(), key.into(), v)),
        }
    }

    pub fn event(&mut self, name: &str, data: String) {
        self.events.push((name.into(), data));
    }

    /// `None`: the key was not staged; `Some(None)`: staged as cleared.
    pub fn get(&self, cat: &str, key: &str) -> Option<Option<&str>> {
        self.sets
            .iter()
            .find(|(c, k, _)| c == cat && k == key)
            .map(|(_, _, v)| v.as_deref())
    }

    pub fn sets(&self) -> &[(String, String, Option<String>)] {
        &self.sets
    }

    pub fn events(&self) -> &[(String, String)] {
        &self.events
    }

    fn merge(&mut self, other: Pass) {
        for (c, k, v) in other.sets {
            self.set(&c, &k, v);
        }
        self.events.extend(other.events);
    }
}

pub trait Task {
    fn name(&self) -> &'static str;
    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError>;
}

struct Slot {
    task: Box<dyn Task>,
    overruns: u32,
    disabled: Option<String>,
    /// Every key this task has staged in a pass that was kept.
    keys: BTreeSet<(String, String)>,
}

/// Stages a clear for every key `slot` has published, once.
fn clear_keys(slot: &mut Slot, pass: &mut Pass) {
    for (cat, key) in std::mem::take(&mut slot.keys) {
        pass.set(&cat, &key, None);
    }
}

pub struct Runner {
    slots: Vec<Slot>,
}

impl Runner {
    pub fn new(tasks: Vec<Box<dyn Task>>) -> Runner {
        Runner {
            slots: tasks
                .into_iter()
                .map(|task| Slot {
                    task,
                    overruns: 0,
                    disabled: None,
                    keys: BTreeSet::new(),
                })
                .collect(),
        }
    }

    /// Runs every enabled task once. `clock` counts ticks of `freq` per second. A task's staged
    /// values and events enter the result only when it returns `Ok`. The pass in which a task
    /// becomes disabled (or the first pass after `disable`) also clears every key that task ever
    /// staged, so no value of an untrusted task stays visible.
    pub fn pass(&mut self, m: &dyn MemRead, clock: &dyn Fn() -> u64, freq: u64) -> Pass {
        let mut pass = Pass::default();
        for slot in &mut self.slots {
            if slot.disabled.is_some() {
                clear_keys(slot, &mut pass);
                continue;
            }
            let start = clock();
            let mut staged = Pass::default();
            let result = catch_unwind(AssertUnwindSafe(|| slot.task.run(m, &mut staged)));
            let took = clock().saturating_sub(start);
            match result {
                Err(_) => slot.disabled = Some("crashed".into()),
                Ok(Err(TaskError::Invalid(why))) => slot.disabled = Some(format!("invalid: {why}")),
                Ok(Err(TaskError::Torn | TaskError::Read(_))) => {}
                Ok(Ok(())) => {
                    slot.keys
                        .extend(staged.sets().iter().map(|(c, k, _)| (c.clone(), k.clone())));
                    pass.merge(staged);
                }
            }
            if slot.disabled.is_none()
                && u128::from(took) * 1000 > u128::from(OVERRUN_MS) * u128::from(freq)
            {
                slot.overruns += 1;
                if slot.overruns > MAX_OVERRUNS {
                    slot.disabled = Some("slow".into());
                }
            }
            if slot.disabled.is_some() {
                clear_keys(slot, &mut pass);
            }
        }
        pass
    }

    /// Switches the task named `task` off as if a run had returned `TaskError::Invalid(why)`.
    /// It never runs again, and the next pass clears every key it published. False if no enabled
    /// task has that name.
    pub fn disable(&mut self, task: &str, why: &str) -> bool {
        let mut found = false;
        for slot in &mut self.slots {
            if slot.disabled.is_none() && slot.task.name() == task {
                slot.disabled = Some(TaskError::Invalid(why.into()).to_string());
                found = true;
            }
        }
        found
    }

    pub fn disabled(&self) -> Vec<(&'static str, String)> {
        self.slots
            .iter()
            .filter_map(|s| s.disabled.clone().map(|why| (s.task.name(), why)))
            .collect()
    }
}
