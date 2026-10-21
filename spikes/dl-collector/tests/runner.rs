use std::cell::Cell;
use std::rc::Rc;

use uw_spike_dlcollector::mem::{FakeMem, MemRead, ReadError};
use uw_spike_dlcollector::runner::{Pass, Runner, Task, TaskError};

type Body = Box<dyn FnMut(&mut Pass) -> Result<(), TaskError>>;

struct Scripted {
    name: &'static str,
    body: Body,
}

impl Task for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&mut self, _m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        (self.body)(out)
    }
}

fn task(
    name: &'static str,
    body: impl FnMut(&mut Pass) -> Result<(), TaskError> + 'static,
) -> Box<dyn Task> {
    Box::new(Scripted {
        name,
        body: Box::new(body),
    })
}

fn setter(name: &'static str, key: &'static str) -> Box<dyn Task> {
    task(name, move |p| {
        p.set("match_info", key, Some("v".into()));
        Ok(())
    })
}

const FREQ: u64 = 1000; // 1 tick = 1 ms

fn now_of(clock: &Rc<Cell<u64>>) -> impl Fn() -> u64 + use<> {
    let c = clock.clone();
    move || c.get()
}

#[test]
fn passes_merge_all_tasks_into_one_pass() {
    let mem = FakeMem::new();
    let mut r = Runner::new(vec![
        setter("a", "roster_0"),
        setter("b", "roster_1"),
        setter("c", "roster_2"),
    ]);
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(p.sets().len(), 3);
    assert!(r.disabled().is_empty());
}

#[test]
fn last_set_of_a_key_wins_and_events_keep_order() {
    let mut p = Pass::default();
    p.set("c", "k", Some("1".into()));
    p.set("c", "k", None);
    p.event("e1", "{}".into());
    p.event("e2", "{}".into());
    assert_eq!(p.get("c", "k"), Some(None));
    assert_eq!(p.sets().len(), 1);
    let names: Vec<_> = p.events().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["e1", "e2"]);
}

/// A task that advances the fake clock by `ms` each run.
fn slow(clock: &Rc<Cell<u64>>, ms: u64) -> Box<dyn Task> {
    let clock = clock.clone();
    task("slow", move |p| {
        clock.set(clock.get() + ms);
        p.set("match_info", "roster_0", Some("slow".into()));
        Ok(())
    })
}

#[test]
fn slow_task_disabled_after_ten_overruns() {
    let mem = FakeMem::new();
    let clock = Rc::new(Cell::new(0u64));
    let mut r = Runner::new(vec![slow(&clock, 25), setter("fast", "roster_1")]);
    let now = now_of(&clock);
    for pass in 1..=10 {
        let p = r.pass(&mem, &now, FREQ);
        assert!(r.disabled().is_empty(), "disabled after pass {pass}");
        assert!(p.get("match_info", "roster_0").is_some());
    }
    let p = r.pass(&mem, &now, FREQ);
    assert_eq!(r.disabled(), vec![("slow", "slow".to_string())]);
    assert!(
        p.get("match_info", "roster_1").is_some(),
        "other tasks still run"
    );
    // Once disabled, it is not run any more.
    let before = clock.get();
    let p = r.pass(&mem, &now, FREQ);
    assert_eq!(clock.get(), before);
    assert!(p.get("match_info", "roster_0").is_none());
    assert!(p.get("match_info", "roster_1").is_some());
}

#[test]
fn twenty_milliseconds_is_not_an_overrun() {
    let mem = FakeMem::new();
    let clock = Rc::new(Cell::new(0u64));
    let mut r = Runner::new(vec![slow(&clock, 20)]);
    let now = now_of(&clock);
    for _ in 0..50 {
        r.pass(&mem, &now, FREQ);
    }
    assert!(r.disabled().is_empty());
}

#[test]
fn fast_passes_between_overruns_do_not_reset_the_count() {
    let mem = FakeMem::new();
    let clock = Rc::new(Cell::new(0u64));
    let n = Rc::new(Cell::new(0u32));
    let (c, k) = (clock.clone(), n.clone());
    // Every other run is slow: 11 overruns need 21 runs.
    let t = task("alt", move |_| {
        k.set(k.get() + 1);
        if k.get() % 2 == 1 {
            c.set(c.get() + 30);
        }
        Ok(())
    });
    let mut r = Runner::new(vec![t]);
    let now = now_of(&clock);
    for _ in 0..19 {
        r.pass(&mem, &now, FREQ);
    }
    assert!(r.disabled().is_empty());
    for _ in 0..2 {
        r.pass(&mem, &now, FREQ);
    }
    assert_eq!(r.disabled(), vec![("alt", "slow".to_string())]);
}

#[test]
fn clock_going_backwards_is_not_an_overrun() {
    let mem = FakeMem::new();
    let clock = Rc::new(Cell::new(1_000_000u64));
    let c = clock.clone();
    let t = task("back", move |_| {
        c.set(c.get() - 5_000);
        Ok(())
    });
    let mut r = Runner::new(vec![t]);
    let now = now_of(&clock);
    for _ in 0..30 {
        r.pass(&mem, &now, FREQ);
    }
    assert!(r.disabled().is_empty());
}

#[test]
fn panicking_task_disabled_immediately() {
    let mem = FakeMem::new();
    let mut r = Runner::new(vec![
        task("boom", |p| {
            p.set("match_info", "roster_0", Some("half".into()));
            panic!("boom")
        }),
        setter("ok", "roster_1"),
    ]);
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(r.disabled(), vec![("boom", "crashed".to_string())]);
    assert!(p.get("match_info", "roster_1").is_some());
    assert!(
        p.get("match_info", "roster_0").is_none(),
        "a crashed task's partial sets are dropped"
    );
    let p = r.pass(&mem, &|| 0, FREQ);
    assert!(p.get("match_info", "roster_1").is_some());
}

#[test]
fn invalid_disables_with_reason_and_drops_sets() {
    let mem = FakeMem::new();
    let mut r = Runner::new(vec![
        task("bad", |p| {
            p.set("match_info", "roster_0", Some("x".into()));
            Err(TaskError::Invalid("layout".into()))
        }),
        setter("ok", "roster_1"),
    ]);
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(r.disabled(), vec![("bad", "invalid: layout".to_string())]);
    assert!(p.get("match_info", "roster_0").is_none());
    assert!(p.get("match_info", "roster_1").is_some());
}

#[test]
fn torn_and_read_errors_drop_only_this_pass() {
    let mem = FakeMem::new();
    let n = Rc::new(Cell::new(0));
    let k = n.clone();
    let flaky = task("flaky", move |p| {
        k.set(k.get() + 1);
        p.set("match_info", "roster_0", Some("x".into()));
        p.event("e", "{}".into());
        match k.get() {
            1 => Err(TaskError::Torn),
            2 => Err(TaskError::Read(ReadError::Unmapped { addr: 1, len: 1 })),
            _ => Ok(()),
        }
    });
    let mut r = Runner::new(vec![flaky, setter("ok", "roster_1")]);
    for _ in 0..2 {
        let p = r.pass(&mem, &|| 0, FREQ);
        assert!(p.get("match_info", "roster_0").is_none());
        assert!(p.events().is_empty());
        assert!(p.get("match_info", "roster_1").is_some());
        assert!(r.disabled().is_empty());
    }
    let p = r.pass(&mem, &|| 0, FREQ);
    assert!(p.get("match_info", "roster_0").is_some());
    assert_eq!(p.events().len(), 1);
}

fn keys_task(
    name: &'static str,
    key: &'static str,
    fail_on: u32,
    fail: fn() -> Result<(), TaskError>,
) -> Box<dyn Task> {
    let mut n = 0;
    task(name, move |p| {
        n += 1;
        p.set("match_info", key, Some(format!("v{n}")));
        if n == fail_on { fail() } else { Ok(()) }
    })
}

#[test]
fn disabling_clears_the_keys_the_task_staged_earlier() {
    fn invalid() -> Result<(), TaskError> {
        Err(TaskError::Invalid("layout".into()))
    }
    fn crash() -> Result<(), TaskError> {
        panic!("boom")
    }
    let mem = FakeMem::new();
    let cases: [(fn() -> Result<(), TaskError>, &str); 2] =
        [(invalid, "invalid: layout"), (crash, "crashed")];
    for (fail, reason) in cases {
        let mut r = Runner::new(vec![
            keys_task("bad", "roster_0", 3, fail),
            setter("ok", "roster_1"),
        ]);
        for _ in 0..2 {
            let p = r.pass(&mem, &|| 0, FREQ);
            assert!(p.get("match_info", "roster_0").unwrap().is_some());
        }
        let p = r.pass(&mem, &|| 0, FREQ);
        assert_eq!(r.disabled(), vec![("bad", reason.to_string())]);
        assert_eq!(p.get("match_info", "roster_0"), Some(None), "{reason}");
        assert!(p.get("match_info", "roster_1").unwrap().is_some());
    }
}

#[test]
fn slow_disable_clears_the_keys_in_the_same_pass() {
    let mem = FakeMem::new();
    let clock = Rc::new(Cell::new(0u64));
    let mut r = Runner::new(vec![slow(&clock, 25)]);
    let now = now_of(&clock);
    for _ in 0..10 {
        let p = r.pass(&mem, &now, FREQ);
        assert!(p.get("match_info", "roster_0").unwrap().is_some());
    }
    let p = r.pass(&mem, &now, FREQ);
    assert_eq!(r.disabled(), vec![("slow", "slow".to_string())]);
    assert_eq!(p.get("match_info", "roster_0"), Some(None));
}

#[test]
fn failed_runs_before_disabling_do_not_add_keys_to_clear() {
    let mem = FakeMem::new();
    let mut r = Runner::new(vec![task("bad", |p| {
        p.set("match_info", "roster_0", Some("x".into()));
        Err(TaskError::Invalid("layout".into()))
    })]);
    let p = r.pass(&mem, &|| 0, FREQ);
    assert!(p.get("match_info", "roster_0").is_none());
}

#[test]
fn a_task_disabled_before_it_runs_never_runs() {
    let mem = FakeMem::new();
    let ran = Rc::new(Cell::new(0));
    let count = ran.clone();
    let mut r = Runner::new(vec![
        task("a", move |p| {
            count.set(count.get() + 1);
            p.set("match_info", "roster_0", Some("v".into()));
            Ok(())
        }),
        setter("b", "roster_1"),
    ]);
    assert!(r.disable("a", "layout: C.m_x"));
    assert!(!r.disable("nope", "x"));
    assert_eq!(
        r.disabled(),
        vec![("a", "invalid: layout: C.m_x".to_string())]
    );
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(ran.get(), 0);
    assert!(p.get("match_info", "roster_0").is_none());
    assert!(p.get("match_info", "roster_1").is_some());
}

#[test]
fn a_task_disabled_after_it_published_has_its_keys_cleared_once() {
    let mem = FakeMem::new();
    let mut r = Runner::new(vec![setter("a", "roster_0"), setter("b", "roster_1")]);
    let p = r.pass(&mem, &|| 0, FREQ);
    assert!(p.get("match_info", "roster_0").unwrap().is_some());

    assert!(r.disable("a", "layout: C.m_x"));
    // Already disabled: the first reason stays.
    assert!(!r.disable("a", "other"));
    assert_eq!(
        r.disabled(),
        vec![("a", "invalid: layout: C.m_x".to_string())]
    );
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(p.get("match_info", "roster_0"), Some(None));
    assert!(p.get("match_info", "roster_1").unwrap().is_some());
    // Cleared once; later passes stage nothing for it.
    let p = r.pass(&mem, &|| 0, FREQ);
    assert_eq!(p.get("match_info", "roster_0"), None);
}
