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
    panic_once_on_commit: bool,
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
        if l.panic_on_commit || std::mem::take(&mut l.panic_once_on_commit) {
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
    assert_eq!(log.events[2].1, "true");
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
    // The phase moves from +0x74 to +0x7c, a word that reads as a valid phase; the layout
    // check catches the move and switches off both tasks that read the field.
    w32(&mut r.mem, world::RULES_OBJ + 0x7c, 3);
    let mut c = r.start("corrupt=C_CitadelGameRules.m_eGameState");
    r.tick(&mut c);
    let why = "invalid: layout: C_CitadelGameRules.m_eGameState";
    assert_eq!(
        r.value("gep_internal", "disabled_tasks"),
        Some(Some(format!(
            r#"[{{"task":"phase","reason":"{why}"}},{{"task":"clock","reason":"{why}"}}]"#
        )))
    );
    assert_eq!(r.value("game_info", "phase"), None);
    assert_eq!(r.value("match_info", "match_clock"), None);
    assert_eq!(r.value("match_info", "match_id"), Some(Some("1234".into())));
    // A name that does not resolve changes nothing.
    let r2 = rig(Skip::default());
    let mut c2 = r2.start("corrupt=C_Nope.m_x");
    r2.tick(&mut c2);
    assert_eq!(
        r2.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
}

/// `corrupt=<arg>` moves a roster field: the roster is listed as disabled from the start with
/// `invalid: layout: <arg>` and publishes nothing; phase, match id and clock keep running.
fn corrupt_roster_field_disables_only_the_roster(arg: &str) {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start(&format!("corrupt={arg}"));
    assert_eq!(c.status(), &Status::Initialized);
    let expect = format!(r#"[{{"task":"roster","reason":"invalid: layout: {arg}"}}]"#);
    assert_eq!(
        r.value("gep_internal", "disabled_tasks"),
        Some(Some(expect.clone()))
    );
    r.tick(&mut c);
    r.tick(&mut c);
    assert_eq!(
        r.value("gep_internal", "disabled_tasks"),
        Some(Some(expect))
    );
    let keys = r.log.borrow().keys();
    assert!(
        !keys
            .iter()
            .any(|k| k.starts_with("match_info.roster_") || k.starts_with("match_info.items_")),
        "{keys:?}"
    );
    assert_eq!(
        r.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
    assert_eq!(r.value("match_info", "match_id"), Some(Some("1234".into())));
    assert!(r.value("match_info", "match_clock").unwrap().is_some());
}

/// C6 Step 5: kills +88 became +96 (the assists) and the roster kept publishing.
#[test]
fn corrupt_kills_disables_the_roster_by_layout() {
    corrupt_roster_field_disables_only_the_roster("PlayerDataGlobal_t.m_iPlayerKills");
}

/// The team id (u8 at +1007) moved onto byte 1015, inside `m_nNextThinkTick`.
#[test]
fn corrupt_team_num_disables_the_roster_by_layout() {
    corrupt_roster_field_disables_only_the_roster("C_BaseEntity.m_iTeamNum");
}

#[test]
fn missing_layout_anchor_disables_its_task() {
    // m_fLevelStartTime anchors the clock's m_flGameStartTime and nothing else.
    let mut r = rig(Skip {
        missing_anchor: Some("m_fLevelStartTime"),
        ..Skip::default()
    });
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    r.tick(&mut c);
    assert_eq!(
        r.value("gep_internal", "disabled_tasks"),
        Some(Some(
            r#"[{"task":"clock","reason":"invalid: layout: C_CitadelGameRules.m_flGameStartTime"}]"#
                .into()
        ))
    );
    assert_eq!(r.value("match_info", "match_clock"), None);
    assert_eq!(r.value("match_info", "game_paused"), None);
    assert_eq!(
        r.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
    assert!(r.value("match_info", "roster_0").unwrap().is_some());
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
    assert_eq!(find("gep_internal", "discovery"), Some(4096));
    assert_eq!(find("game_info", "phase"), Some(64));
    assert_eq!(find("game_info", "game_mode"), Some(256));
    assert_eq!(find("match_info", "match_id"), Some(32));
    assert_eq!(find("match_info", "match_clock"), Some(16));
    assert_eq!(find("match_info", "game_paused"), Some(8));
    assert_eq!(find("match_info", "team_score"), Some(64));
    assert_eq!(find("match_info", "match_outcome"), Some(64));
    for i in 0..12 {
        assert_eq!(find("match_info", &format!("roster_{i}")), Some(1024));
        assert_eq!(find("match_info", &format!("items_{i}")), Some(2048));
    }
    assert_eq!(find("match_info", "roster_12"), None);
    assert_eq!(s.keys().count(), 6 + 2 + 5 + 24);
}

#[test]
fn main_loop_not_found_publishes_error_and_stops_passes() {
    let r = rig(Skip::default());
    let mut c = r.start("");
    assert_eq!(c.status(), &Status::Initialized);
    c.fail("main-loop caller not found");
    assert_eq!(
        c.status(),
        &Status::Error("main-loop caller not found".into())
    );
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("error".into()))
    );
    assert_eq!(
        r.value("gep_internal", "plugin_error"),
        Some(Some("main-loop caller not found".into()))
    );
    let commits = r.log.borrow().commits.len();
    r.now.fetch_add(100, Ordering::Relaxed);
    r.tick(&mut c);
    c.check_stall();
    assert_eq!(r.log.borrow().commits.len(), commits);
}

#[test]
fn modules_without_the_main_loop_caller_fail_the_collector() {
    // The plugin's start path: tier0.dll and engine2.dll are loaded but hold neither pattern.
    let r = rig(Skip::default());
    let mut c = r.start("");
    assert_eq!(c.status(), &Status::Initialized);
    let named = |name: &str| Module {
        name: name.into(),
        ..r.module.clone()
    };
    let modules = [r.module.clone(), named("engine2.dll"), named("tier0.dll")];
    assert_eq!(c.main_loop_filter(&r.mem, &modules), None);
    assert_eq!(
        c.status(),
        &Status::Error("main-loop caller not found".into())
    );
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("error".into()))
    );
    assert_eq!(
        r.value("gep_internal", "plugin_error"),
        Some(Some("main-loop caller not found".into()))
    );
}

#[test]
fn scanning_is_published_once_the_hook_is_armed() {
    let r = rig(Skip::default());
    let mut c = r.start("");
    c.scanning();
    assert_eq!(c.status(), &Status::Scanning);
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("scanning".into()))
    );
    // Arming an errored collector changes nothing.
    let r = rig(Skip {
        rules: true,
        ..Skip::default()
    });
    let mut c = r.start("");
    c.scanning();
    assert!(matches!(c.status(), Status::Error(_)));
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("error".into()))
    );
}

#[test]
fn no_pass_for_five_seconds_is_stalled_until_the_next_pass() {
    let r = rig(Skip::default());
    let mut c = r.start("");
    c.scanning();
    r.now.store(4_999, Ordering::Relaxed);
    c.check_stall();
    assert_eq!(c.status(), &Status::Scanning);
    r.now.store(5_000, Ordering::Relaxed);
    c.check_stall();
    assert_eq!(c.status(), &Status::Stalled);
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("stalled".into()))
    );
    let commits = r.log.borrow().commits.len();
    c.check_stall();
    assert_eq!(r.log.borrow().commits.len(), commits, "stalled once");

    r.tick(&mut c);
    assert_eq!(c.status(), &Status::Scanning);
    assert_eq!(
        r.value("gep_internal", "plugin_status"),
        Some(Some("scanning".into()))
    );
    // The stall window restarts at the last pass.
    r.now.store(9_000, Ordering::Relaxed);
    c.check_stall();
    assert_eq!(c.status(), &Status::Scanning);
    r.now.store(10_000, Ordering::Relaxed);
    c.check_stall();
    assert_eq!(c.status(), &Status::Stalled);
}

#[test]
fn stall_check_needs_scanning() {
    let r = rig(Skip::default());
    let mut c = r.start("");
    r.now.store(60_000, Ordering::Relaxed);
    c.check_stall();
    assert_eq!(c.status(), &Status::Initialized);
}

fn keys_of(commit: &[Set]) -> Vec<(&str, Option<&[u8]>)> {
    commit
        .iter()
        .map(|(_, k, v)| (k.as_str(), v.as_deref()))
        .collect()
}

#[test]
fn disabling_the_clock_clears_its_keys_in_that_pass() {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    r.tick(&mut c);
    assert!(r.value("match_info", "match_clock").unwrap().is_some());
    assert!(r.value("match_info", "game_paused").unwrap().is_some());

    world::w64(&mut r.mem, world::VTABLE + 0x220, 0x9999_9999);
    r.tick(&mut c);
    let log = r.log.borrow();
    let last = log.commits.last().unwrap();
    let k = keys_of(last);
    assert!(k.contains(&("match_clock", None)), "{k:?}");
    assert!(k.contains(&("game_paused", None)), "{k:?}");
    let d = log
        .value("gep_internal", "disabled_tasks")
        .unwrap()
        .unwrap();
    assert!(d.contains(r#""task":"clock","reason":"invalid: "#), "{d}");
    // The other tasks' values stay.
    assert_eq!(
        log.value("game_info", "phase"),
        Some(Some("GameInProgress".into()))
    );
    assert!(log.value("match_info", "roster_0").unwrap().is_some());
}

#[test]
fn disabling_the_roster_clears_roster_and_items_keys() {
    let mut r = rig(Skip::default());
    let mut teams = teams_of(&[2, 1]);
    teams[0][0].items = vec![5, 6];
    r.world.set_players(&mut r.mem, &teams);
    let mut c = r.start("");
    r.tick(&mut c);
    assert!(r.value("match_info", "items_0").unwrap().is_some());

    teams[0][0].local = true;
    teams[1][0].local = true;
    r.world.set_players(&mut r.mem, &teams);
    r.tick(&mut c);
    let log = r.log.borrow();
    let k = keys_of(log.commits.last().unwrap());
    for i in 0..3 {
        assert!(k.contains(&(format!("roster_{i}").as_str(), None)), "{k:?}");
        assert!(k.contains(&(format!("items_{i}").as_str(), None)), "{k:?}");
    }
    assert!(log.value("match_info", "match_clock").unwrap().is_some());
}

#[test]
fn panic_in_tick_clears_game_keys_with_the_error_status() {
    let mut r = rig(Skip::default());
    r.world.set_players(&mut r.mem, &teams_of(&[1, 1]));
    let mut c = r.start("");
    r.tick(&mut c);
    assert!(r.value("game_info", "phase").unwrap().is_some());
    let before = r.log.borrow().commits.len();

    // The next commit panics once; the collector fails and clears what it had published.
    r.log.borrow_mut().panic_once_on_commit = true;
    w32(&mut r.mem, world::RULES_OBJ + world::OFF_STATE, 8);
    r.tick(&mut c);
    assert!(matches!(c.status(), Status::Error(_)));
    let log = r.log.borrow();
    assert_eq!(
        log.commits.len(),
        before + 1,
        "one commit: the error commit"
    );
    let k = keys_of(log.commits.last().unwrap());
    assert!(
        k.contains(&("plugin_status", Some(b"error".as_slice()))),
        "{k:?}"
    );
    for key in [
        "phase",
        "match_id",
        "match_clock",
        "game_paused",
        "roster_0",
        "roster_1",
        "items_0",
    ] {
        assert!(k.contains(&(key, None)), "{key} not cleared: {k:?}");
    }
    assert_eq!(log.value("game_info", "phase"), Some(None));
    assert_eq!(log.value("match_info", "roster_0"), Some(None));
    // Status keys are not cleared.
    assert!(log.value("gep_internal", "version_info").unwrap().is_some());
}

#[test]
fn a_64_item_inventory_with_a_long_name_fits() {
    let mut r = rig(Skip::default());
    let name = "\u{1}".repeat(127);
    let mut teams = teams_of(&[1, 1]);
    teams[0][0].name = name;
    teams[0][0].items = (0..64).map(|i| 4_000_000_000 + i).collect();
    r.world.set_players(&mut r.mem, &teams);
    let mut c = r.start("");
    r.tick(&mut c);
    let items = r.value("match_info", "items_0").unwrap().unwrap();
    assert!(items.len() > 1024 && items.len() <= 2048, "{}", items.len());
    assert!(r.value("match_info", "roster_0").unwrap().unwrap().len() <= 1024);
    assert!(r.value("match_info", "roster_1").unwrap().is_some());
    let d = r.value("gep_internal", "disabled_tasks").unwrap().unwrap();
    assert_eq!(d, "[]");
}
