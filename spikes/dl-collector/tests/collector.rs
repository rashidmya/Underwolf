mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use common::world::{self, Player, Skip, World, image, teams_of, w8, w32, wf};
use uw_spike_dlcollector::collector::{ChannelOut, Collector, Status};
use uw_spike_dlcollector::keys::deadlock_schema;
use uw_spike_dlcollector::mem::FakeMem;
use uw_spike_dlcollector::module::Module;

type Set = (String, String, Option<Vec<u8>>);

#[derive(Default)]
struct Log {
    commits: Vec<Vec<Set>>,
    events: Vec<(String, String)>,
    /// Scripted answers for `push_event` (`false` = throttled); empty = accept.
    answers: VecDeque<bool>,
    attempts: Vec<String>,
    panic_on_commit: bool,
}

impl Log {
    fn value(&self, cat: &str, key: &str) -> Option<Option<String>> {
        self.commits
            .iter()
            .flatten()
            .rev()
            .find(|(c, k, _)| c == cat && k == key)
            .map(|(_, _, v)| v.as_ref().map(|v| String::from_utf8(v.clone()).unwrap()))
    }

    fn keys(&self) -> Vec<String> {
        let mut k: Vec<_> = self
            .commits
            .iter()
            .flatten()
            .map(|(c, k, _)| format!("{c}.{k}"))
            .collect();
        k.sort();
        k.dedup();
        k
    }
}

struct Out(Rc<RefCell<Log>>);

impl ChannelOut for Out {
    fn commit(&mut self, sets: &[Set], _ts: u64) {
        let mut l = self.0.borrow_mut();
        if l.panic_on_commit {
            panic!("commit failed");
        }
        l.commits.push(sets.to_vec());
    }

    fn push_event(&mut self, name: &str, data: &[u8], _ts: u64) -> bool {
        let mut l = self.0.borrow_mut();
        l.attempts.push(name.into());
        if l.answers.pop_front() == Some(false) {
            return false;
        }
        l.events
            .push((name.into(), String::from_utf8(data.to_vec()).unwrap()));
        true
    }
}

const FREQ: u64 = 1000;

struct Rig {
    mem: FakeMem,
    module: Module,
    world: World,
    log: Rc<RefCell<Log>>,
    now: Arc<AtomicU64>,
}

fn rig(skip: Skip) -> Rig {
    let (mem, module, world) = image(skip);
    Rig {
        mem,
        module,
        world,
        log: Rc::default(),
        now: Arc::new(AtomicU64::new(0)),
    }
}

impl Rig {
    fn start(&self, args: &str) -> Collector {
        let now = self.now.clone();
        Collector::start(
            &self.mem,
            &self.module,
            Box::new(Out(self.log.clone())),
            args,
            Box::new(move || now.load(Ordering::Relaxed)),
            FREQ,
        )
    }

    fn tick(&self, c: &mut Collector) {
        c.tick(&self.mem);
    }

    fn value(&self, cat: &str, key: &str) -> Option<Option<String>> {
        self.log.borrow().value(cat, key)
    }
}

#[test]
fn unresolved_required_root_publishes_only_error_status() {
    let r = rig(Skip {
        rules: true,
        ..Skip::default()
    });
    let mut c = r.start("");
    let Status::Error(why) = c.status().clone() else {
        panic!("expected Error, got {:?}", c.status());
    };
    assert!(why.contains("game rules"), "{why}");
    assert_eq!(
        r.log.borrow().keys(),
        ["gep_internal.plugin_error", "gep_internal.plugin_status"]
    );
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("error".into()))
    );
    assert_eq!(r.value("gep_internal", "plugin_error"), Some(Some(why)));
    let commits = r.log.borrow().commits.len();
    for _ in 0..3 {
        r.now.fetch_add(100, Ordering::Relaxed);
        r.tick(&mut c);
    }
    assert_eq!(r.log.borrow().commits.len(), commits);
    assert!(r.log.borrow().attempts.is_empty());
}

#[test]
fn missing_required_field_publishes_only_error_status() {
    let r = rig(Skip {
        match_id_field: true,
        ..Skip::default()
    });
    let c = r.start("");
    let Status::Error(why) = c.status() else {
        panic!("expected Error");
    };
    assert!(why.contains("m_unMatchID"), "{why}");
    assert_eq!(
        r.log.borrow().keys(),
        ["gep_internal.plugin_error", "gep_internal.plugin_status"]
    );
}

#[test]
fn missing_clock_root_disables_only_clock() {
    let mut r = rig(Skip {
        clock: true,
        ..Skip::default()
    });
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    assert_eq!(c.status(), &Status::Initialized);
    r.tick(&mut c);
    let d = r.value("gep_internal", "disabled_tasks").unwrap().unwrap();
    assert!(
        d.contains(r#"{"task":"clock","reason":"unresolved: "#),
        "{d}"
    );
    assert_eq!(
        r.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
    assert!(r.value("match_info", "roster_0").unwrap().is_some());
    assert_eq!(r.value("match_info", "match_clock"), None);
}

#[test]
fn unresolved_roster_field_disables_only_roster() {
    let r = rig(Skip {
        roster_class: true,
        ..Skip::default()
    });
    let mut c = r.start("");
    r.tick(&mut c);
    let d = r.value("gep_internal", "disabled_tasks").unwrap().unwrap();
    assert!(
        d.contains(r#"{"task":"roster","reason":"unresolved: "#),
        "{d}"
    );
    // The clock's game-rules fields are missing in this schema as well; the rest keeps running.
    assert!(
        d.contains(r#"{"task":"clock","reason":"unresolved: "#),
        "{d}"
    );
    assert_eq!(
        r.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
}

#[test]
fn one_commit_per_pass() {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[6, 6]));
    let mut c = r.start("");
    let at_start = r.log.borrow().commits.len();
    assert_eq!(at_start, 1, "start publishes its status in one commit");
    r.tick(&mut c);
    assert_eq!(r.log.borrow().commits.len(), at_start + 1);
    let last = r.log.borrow().commits.last().unwrap().clone();
    // phase, match_id, clock, game_paused and the roster all arrive together.
    let keys: Vec<_> = last.iter().map(|(_, k, _)| k.as_str()).collect();
    for k in [
        "phase",
        "match_id",
        "match_clock",
        "game_paused",
        "roster_0",
        "items_11",
        "plugin_status",
    ] {
        assert!(keys.contains(&k), "{k} missing from {keys:?}");
    }
    assert_eq!(c.status(), &Status::Scanning);
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("scanning".into()))
    );
}

#[test]
fn unchanged_pass_does_not_commit() {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    r.tick(&mut c);
    let n = r.log.borrow().commits.len();
    r.tick(&mut c);
    assert_eq!(r.log.borrow().commits.len(), n);
    // Only the changed key is committed next.
    w32(&mut r.mem, world::RULES_OBJ + world::OFF_STATE, 8);
    r.tick(&mut c);
    let log = r.log.borrow();
    assert_eq!(log.commits.len(), n + 1);
    let last = log.commits.last().unwrap();
    // The phase, and the two clock keys that clear outside GameInProgress.
    assert_eq!(last.len(), 3, "{last:?}");
    assert!(
        last.iter()
            .any(|(_, k, v)| k == "phase" && v.as_deref() == Some(b"PostGame".as_slice()))
    );
}

#[test]
fn start_publishes_version_info() {
    let r = rig(Skip::default());
    let _c = r.start("");
    let v = r.value("gep_internal", "version_info").unwrap().unwrap();
    assert_eq!(
        v,
        format!(
            r#"{{"collector":"uw-spike-dlcollector {}","schema_layout":"disk-class-v2"}}"#,
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("initialized".into()))
    );
}

#[test]
fn throttled_events_are_retried_in_order() {
    let mut r = rig(Skip::default());
    let mut c = r.start("");
    r.tick(&mut c); // first clock event goes through
    assert_eq!(r.log.borrow().events.len(), 1);
    // The next two offers are refused.
    r.log.borrow_mut().answers = VecDeque::from([false, false]);

    // Pausing at tick 16000 emits `clock` then `game_paused`; the first is refused, so the
    // second is not even offered (order is kept).
    w8(&mut r.mem, world::RULES_OBJ + world::OFF_PAUSED, 1);
    w32(&mut r.mem, world::RULES_OBJ + world::OFF_PAUSE_START, 16000);
    r.now.fetch_add(60, Ordering::Relaxed);
    r.tick(&mut c);
    assert_eq!(r.log.borrow().events.len(), 1);
    assert_eq!(r.log.borrow().attempts[1..], ["clock"]);

    // Refused again: the same event is offered first.
    r.now.fetch_add(60, Ordering::Relaxed);
    r.tick(&mut c);
    assert_eq!(r.log.borrow().events.len(), 1);
    assert_eq!(r.log.borrow().attempts[1..], ["clock", "clock"]);

    // Accepted now: both go out, in order.
    r.now.fetch_add(60, Ordering::Relaxed);
    r.tick(&mut c);
    let log = r.log.borrow();
    let names: Vec<_> = log.events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["clock", "clock", "game_paused"]);
    assert_eq!(log.events[1].1, r#"{"match_clock":"07:40"}"#);
    assert_eq!(log.events[2].1, r#"{"game_paused":true}"#);
}

#[test]
fn backlog_keeps_new_events_behind_old_ones() {
    let mut r = rig(Skip::default());
    let mut c = r.start("");
    r.log.borrow_mut().answers = VecDeque::from([false]);
    r.tick(&mut c); // `clock` 08:15 refused
    assert!(r.log.borrow().events.is_empty());
    wf(&mut r.mem, world::CLOCK_GLOBALS + 0x30, 600.0);
    r.now.fetch_add(60, Ordering::Relaxed);
    r.tick(&mut c); // retried first, then the new one
    let log = r.log.borrow();
    let data: Vec<_> = log.events.iter().map(|(_, d)| d.as_str()).collect();
    assert_eq!(
        data,
        [r#"{"match_clock":"08:15"}"#, r#"{"match_clock":"09:20"}"#]
    );
}

#[test]
fn event_throttle_limits_to_thirty_per_window() {
    let mut r = rig(Skip::default());
    let mut c = r.start("");
    // One clock event per tick (the clock moves 1 s each tick), all inside one 200 ms window.
    let mut t = 535.0f32;
    for _ in 0..40 {
        t += 1.0;
        wf(&mut r.mem, world::CLOCK_GLOBALS + 0x30, t);
        r.tick(&mut c);
    }
    assert_eq!(r.log.borrow().events.len(), 30);
    // A window later the backlog drains.
    r.now.fetch_add(250, Ordering::Relaxed);
    r.tick(&mut c);
    assert_eq!(r.log.borrow().events.len(), 40);
}

#[test]
fn pass_stats_publish_every_ten_seconds() {
    let r = rig(Skip::default());
    let mut c = r.start("");
    r.tick(&mut c);
    assert_eq!(r.value("gep_internal", "pass_stats"), None);
    for _ in 0..5 {
        r.now.fetch_add(1000, Ordering::Relaxed);
        r.tick(&mut c);
    }
    assert_eq!(r.value("gep_internal", "pass_stats"), None);
    r.now.fetch_add(5000, Ordering::Relaxed);
    r.tick(&mut c);
    let s = r.value("gep_internal", "pass_stats").unwrap().unwrap();
    assert!(s.starts_with(r#"{"median_ms":"#), "{s}");
    assert!(
        s.contains(r#""max_ms":"#) && s.ends_with(r#""passes":7}"#),
        "{s}"
    );
}

#[test]
fn selftest_skips_roots_and_emits_one_event() {
    // No roots exist in this image at all.
    let r = rig(Skip {
        rules: true,
        clock: true,
        ..Skip::default()
    });
    let mut c = r.start("discover=5 selftest=1 unknown=x");
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("selftest".into()))
    );
    assert_eq!(
        r.log.borrow().events,
        [("selftest".to_string(), "{}".to_string())]
    );
    let commits = r.log.borrow().commits.len();
    for _ in 0..3 {
        r.now.fetch_add(100, Ordering::Relaxed);
        r.tick(&mut c);
    }
    assert_eq!(r.log.borrow().commits.len(), commits);
    assert_eq!(r.log.borrow().events.len(), 1);
    assert!(r.value("gep_internal", "plugin_error").is_none());
}

#[test]
fn unknown_args_are_ignored() {
    let r = rig(Skip::default());
    let c = r.start("foo=bar discover=3 lonely corrupt= =x");
    assert_eq!(c.status(), &Status::Initialized);
}

#[test]
fn corrupt_arg_shifts_the_named_offset() {
    let mut r = rig(Skip::default());
    // The phase moves from +0x74 to +0x7c: the collector reads the wrong word.
    w32(&mut r.mem, world::RULES_OBJ + 0x7c, 3);
    let mut c = r.start("corrupt=C_CitadelGameRules.m_eGameState");
    r.tick(&mut c);
    assert_eq!(
        r.value("game_info", "phase"),
        Some(Some("HeroSelection".into()))
    );
    // A name that does not resolve changes nothing.
    let r2 = rig(Skip::default());
    let mut c2 = r2.start("corrupt=C_Nope.m_x");
    r2.tick(&mut c2);
    assert_eq!(
        r2.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
}

#[test]
fn nothing_panics_out_of_tick() {
    let mut r = rig(Skip::default());
    r.world
        .set_players(&mut r.mem, &[vec![Player::new("A", 1, 1)], vec![]]);
    let mut c = r.start("");
    r.log.borrow_mut().panic_on_commit = true;
    r.tick(&mut c);
    assert!(matches!(c.status(), Status::Error(_)), "{:?}", c.status());
    // Later ticks do nothing.
    r.log.borrow_mut().panic_on_commit = false;
    let n = r.log.borrow().commits.len();
    r.tick(&mut c);
    assert_eq!(r.log.borrow().commits.len(), n);
}

#[test]
fn torn_roster_publishes_other_tasks_and_keeps_old_roster() {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    r.tick(&mut c);
    let before = r.value("match_info", "roster_0");
    // The team array now holds an implausible pointer: roster reads fail, phase keeps working.
    world::w64(&mut r.mem, world::TEAM_ARRAY, 5);
    w32(&mut r.mem, world::RULES_OBJ + world::OFF_STATE, 8);
    r.tick(&mut c);
    assert_eq!(r.value("match_info", "roster_0"), before);
    assert_eq!(r.value("game_info", "phase"), Some(Some("PostGame".into())));
    assert_eq!(
        r.value("gep_internal", "disabled_tasks"),
        Some(Some("[]".into()))
    );
}

#[test]
fn schema_has_the_fixed_keys_and_limits() {
    let s = deadlock_schema();
    let find = |c: &str, k: &str| {
        s.keys()
            .find(|(cc, kk, _)| *cc == c && *kk == k)
            .map(|x| x.2)
    };
    assert_eq!(find("gep_internal", "version_info"), Some(128));
    assert_eq!(find("gep_internal", "plugin_status"), Some(32));
    assert_eq!(find("gep_internal", "plugin_error"), Some(256));
    assert_eq!(find("gep_internal", "disabled_tasks"), Some(1024));
    assert_eq!(find("gep_internal", "pass_stats"), Some(128));
    assert_eq!(find("game_info", "phase"), Some(64));
    assert_eq!(find("game_info", "game_mode"), Some(256));
    assert_eq!(find("match_info", "match_id"), Some(32));
    assert_eq!(find("match_info", "match_clock"), Some(16));
    assert_eq!(find("match_info", "game_paused"), Some(8));
    assert_eq!(find("match_info", "team_score"), Some(64));
    assert_eq!(find("match_info", "match_outcome"), Some(64));
    for i in 0..12 {
        assert_eq!(find("match_info", &format!("roster_{i}")), Some(1024));
        assert_eq!(find("match_info", &format!("items_{i}")), Some(1024));
    }
    assert_eq!(find("match_info", "roster_12"), None);
    assert_eq!(s.keys().count(), 5 + 2 + 5 + 24);
}
