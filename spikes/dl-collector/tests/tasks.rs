mod common;

use common::world::{self, Player, World, teams_of, w8, w32, wf};
use common::{Hooked, Synth};
use uw_spike_dlcollector::keys::deadlock_schema;
use uw_spike_dlcollector::mem::{FakeMem, MemRead, ReadError};
use uw_spike_dlcollector::runner::{Pass, Runner, Task, TaskError};
use uw_spike_dlcollector::tasks::build;

fn fresh() -> (FakeMem, World, Vec<Box<dyn Task>>) {
    let (mem, w) = World::standalone();
    let built = build(&w.roots, Ok(w.clock.clone()), &world::fields()).unwrap();
    assert!(built.unresolved.is_empty(), "{:?}", built.unresolved);
    (mem, w, built.tasks)
}

fn task(tasks: &mut Vec<Box<dyn Task>>, name: &str) -> Box<dyn Task> {
    let i = tasks.iter().position(|t| t.name() == name).unwrap();
    tasks.remove(i)
}

fn run(t: &mut Box<dyn Task>, m: &dyn MemRead) -> Result<Pass, TaskError> {
    let mut p = Pass::default();
    t.run(m, &mut p).map(|()| p)
}

fn value<'a>(p: &'a Pass, cat: &str, key: &str) -> Option<&'a str> {
    p.get(cat, key)
        .unwrap_or_else(|| panic!("{cat}.{key} not staged"))
}

fn staged(p: &Pass, cat: &str, key: &str) -> bool {
    p.get(cat, key).is_some()
}

#[test]
fn phase_and_match_id_publish_names() {
    let (mem, _, mut tasks) = fresh();
    let mut phase = task(&mut tasks, "phase");
    let mut id = task(&mut tasks, "match_id");
    let p = run(&mut phase, &mem).unwrap();
    assert_eq!(value(&p, "game_info", "phase"), Some("GameInProgress"));
    assert_eq!(
        value(&run(&mut id, &mem).unwrap(), "match_info", "match_id"),
        Some("1234")
    );
}

#[test]
fn phase_names_cover_every_state_and_out_of_range_is_invalid() {
    let (mut mem, w, mut tasks) = fresh();
    let mut phase = task(&mut tasks, "phase");
    let names = [
        "Invalid",
        "Init",
        "WaitingForPlayersToJoin",
        "HeroSelection",
        "MatchIntro",
        "WaitForMapToLoad",
        "PreGameWait",
        "GameInProgress",
        "PostGame",
        "PostGame_PlayOfTheGame",
        "Abandoned",
        "End",
    ];
    for (i, name) in names.iter().enumerate() {
        w.set_state(&mut mem, i as i32);
        let p = run(&mut phase, &mem).unwrap();
        assert_eq!(value(&p, "game_info", "phase"), Some(*name));
    }
    for bad in [12, -1, i32::MAX] {
        w.set_state(&mut mem, bad);
        let p = run(&mut phase, &mem).unwrap();
        assert_eq!(value(&p, "game_info", "phase"), Some("Invalid"));
    }
}

#[test]
fn match_id_is_unsigned_and_zero_is_unset() {
    let (mut mem, _, mut tasks) = fresh();
    let mut id = task(&mut tasks, "match_id");
    w32(
        &mut mem,
        world::RULES_OBJ + world::OFF_MATCH_ID,
        0xffff_fff0,
    );
    let p = run(&mut id, &mem).unwrap();
    assert_eq!(value(&p, "match_info", "match_id"), Some("4294967280"));
    w32(&mut mem, world::RULES_OBJ + world::OFF_MATCH_ID, 0);
    let p = run(&mut id, &mem).unwrap();
    assert_eq!(value(&p, "match_info", "match_id"), None);
}

#[test]
fn null_game_rules_clears_match_keys() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 1]));
    for name in ["phase", "match_id", "clock", "roster"] {
        let mut t = task(&mut tasks, name);
        let p = run(&mut t, &mem).unwrap();
        // A valid pass first: the keys carry values.
        let key = match name {
            "phase" => ("game_info", "phase"),
            "match_id" => ("match_info", "match_id"),
            "clock" => ("match_info", "match_clock"),
            _ => ("match_info", "roster_0"),
        };
        assert!(value(&p, key.0, key.1).is_some(), "{name}");
        w.rules_present(&mut mem, false);
        let p = run(&mut t, &mem).unwrap();
        assert_eq!(value(&p, key.0, key.1), None, "{name}");
        if name == "roster" {
            for i in 0..12 {
                assert_eq!(value(&p, "match_info", &format!("roster_{i}")), None);
                assert_eq!(value(&p, "match_info", &format!("items_{i}")), None);
            }
        }
        w.rules_present(&mut mem, true);
    }
}

#[test]
fn roster_reads_twelve_players_and_one_local() {
    let (mut mem, w, mut tasks) = fresh();
    let mut teams = teams_of(&[6, 6]);
    teams[1][3] = teams[1][3].clone().local();
    teams[0][0].kills = 3;
    teams[0][0].deaths = 1;
    teams[0][0].assists = 5;
    teams[0][0].items = vec![11, 22];
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    let mut locals = 0;
    for i in 0..12 {
        let v = value(&p, "match_info", &format!("roster_{i}")).unwrap();
        locals += v.matches("\"is_local\":true").count();
        assert!(
            value(&p, "match_info", &format!("items_{i}")).is_some(),
            "items_{i}"
        );
    }
    assert_eq!(locals, 1);
    assert_eq!(
        value(&p, "match_info", "roster_0"),
        Some(concat!(
            r#"{"player_name":"P0_0","steam_id":"76561198000000001","hero_id":1,"team_id":2,"#,
            r#""is_local":false,"kills":3,"deaths":1,"assists":5}"#
        ))
    );
    assert_eq!(
        value(&p, "match_info", "items_0"),
        Some(
            r#"{"player_name":"P0_0","steam_id":"76561198000000001","items":[{"id":11},{"id":22}]}"#
        )
    );
    // A second team's player carries that team's id; the steam id stays a string.
    let v = value(&p, "match_info", "roster_9").unwrap();
    assert!(v.contains(r#""steam_id":"76561198000000010""#), "{v}");
    assert!(v.contains(r#""team_id":3"#), "{v}");
}

#[test]
fn player_names_are_json_escaped() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(
        &mut mem,
        &[vec![Player::new("a\"b\\c\u{1}é", 7, 1)], vec![]],
    );
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    let v = value(&p, "match_info", "roster_0").unwrap();
    assert!(v.starts_with(r#"{"player_name":"a\"b\\c\u0001é","#), "{v}");
}

#[test]
fn hero_zero_is_not_a_roster_entry() {
    let (mut mem, w, mut tasks) = fresh();
    let mut teams = teams_of(&[2, 0]);
    teams[0][0].hero = 0;
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    assert!(
        value(&p, "match_info", "roster_0")
            .unwrap()
            .contains("P0_1")
    );
    assert_eq!(value(&p, "match_info", "roster_1"), None);
}

#[test]
fn stale_handle_is_skipped() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[2, 0]));
    // Player 0's entry now carries another serial.
    let entry = world::PAGE0 + (0x8002u64 & 0x1ff) * world::STRIDE;
    w32(&mut mem, entry + world::SERIAL, 0x8003 + 0x8000);
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    let v = value(&p, "match_info", "roster_0").unwrap();
    assert!(v.contains("P0_1"), "{v}");
    assert_eq!(value(&p, "match_info", "roster_1"), None);
    assert!(!(0..12).any(|i| {
        value(&p, "match_info", &format!("roster_{i}")).is_some_and(|v| v.contains("P0_0"))
    }));
}

#[test]
fn roster_change_during_read_is_discarded() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[2, 1]));
    let handles = World::handles(0);
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        // After the first read of team 0's handle data, a player leaves.
        if addr == handles && len == 8 && !fired {
            fired = true;
            w32(mem, handles, 0xffff_ffff);
        }
    });
    let mut roster = task(&mut tasks, "roster");
    let mut p = Pass::default();
    assert_eq!(roster.run(&hooked, &mut p), Err(TaskError::Torn));
    assert!(!staged(&p, "match_info", "roster_0"));
    // Without the interference the next pass succeeds.
    assert!(run(&mut roster, &hooked).is_ok());
}

#[test]
fn items_reallocation_is_discarded() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(
        &mut mem,
        &[vec![Player::new("Al", 5, 3).items(&[1, 2, 3])], vec![]],
    );
    let header = World::upgrades_header(0);
    let items = world::UPGRADES;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        // After the item ids are read, the upgrades vector moves to a new allocation.
        if addr == items && len == 12 && !fired {
            fired = true;
            w32(mem, header, 3);
            world::w64(mem, header + 8, items + 0x80);
        }
    });
    let mut roster = task(&mut tasks, "roster");
    let mut p = Pass::default();
    assert_eq!(roster.run(&hooked, &mut p), Err(TaskError::Torn));
    assert!(!staged(&p, "match_info", "items_0"));
    assert!(!staged(&p, "match_info", "roster_0"));
}

#[test]
fn item_contents_changing_during_read_is_discarded() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(
        &mut mem,
        &[vec![Player::new("Al", 5, 3).items(&[1, 2, 3])], vec![]],
    );
    let items = world::UPGRADES;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        if addr == items && len == 12 && !fired {
            fired = true;
            w32(mem, items, 99);
        }
    });
    let mut roster = task(&mut tasks, "roster");
    assert_eq!(
        roster.run(&hooked, &mut Pass::default()),
        Err(TaskError::Torn)
    );
}

#[test]
fn two_local_players_is_invalid() {
    let (mut mem, w, mut tasks) = fresh();
    let mut teams = teams_of(&[2, 1]);
    teams[0][0].local = true;
    teams[1][0].local = true;
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    let mut p = Pass::default();
    let Err(TaskError::Invalid(why)) = roster.run(&mem, &mut p) else {
        panic!("expected Invalid");
    };
    assert!(why.contains("local"), "{why}");
    // Under a runner the task is disabled with an `invalid: …` reason.
    let mut runner = Runner::new(vec![roster]);
    runner.pass(&mem, &|| 0, 1000);
    let d = runner.disabled();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].0, "roster");
    assert!(d[0].1.starts_with("invalid: "), "{:?}", d[0]);
}

#[test]
fn duplicate_identity_and_thirteen_players_are_invalid() {
    let (mut mem, w, mut tasks) = fresh();
    let mut teams = teams_of(&[2, 0]);
    teams[0][1].name = teams[0][0].name.clone();
    teams[0][1].steam = teams[0][0].steam;
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Invalid(_))
    ));

    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[7, 6]));
    let mut roster = task(&mut tasks, "roster");
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Invalid(_))
    ));
}

#[test]
fn bad_local_flag_is_invalid_and_torn_flag_is_not() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 0]));
    w8(&mut mem, World::controller(0) + world::OFF_LOCAL, 7);
    let mut roster = task(&mut tasks, "roster");
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Invalid(_))
    ));

    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 0]));
    let flag = World::controller(0) + world::OFF_LOCAL;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        if addr == flag && len == 1 && !fired {
            fired = true;
            w8(mem, flag, 1);
        }
    });
    let mut roster = task(&mut tasks, "roster");
    assert_eq!(
        roster.run(&hooked, &mut Pass::default()),
        Err(TaskError::Torn)
    );
}

#[test]
fn unterminated_or_invalid_name_is_a_read_error() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 0]));
    let name = World::controller(0) + world::OFF_NAME;
    assert!(mem.write(name, &[b'A'; 128]));
    let mut roster = task(&mut tasks, "roster");
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Read(ReadError::Unterminated))
    ));
    assert!(mem.write(name, &[0xff, 0]));
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Read(ReadError::NotUtf8))
    ));
}

#[test]
fn implausible_team_count_is_a_read_error() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 0]));
    w32(&mut mem, w.roots.teams, 100_000);
    let mut roster = task(&mut tasks, "roster");
    assert!(matches!(
        roster.run(&mem, &mut Pass::default()),
        Err(TaskError::Read(_))
    ));
}

#[test]
fn removed_player_clears_slot() {
    let (mut mem, w, mut tasks) = fresh();
    let teams = teams_of(&[2, 2]);
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    let slot_of = |p: &Pass, who: &str| {
        (0..12).find(|i| {
            value(p, "match_info", &format!("roster_{i}"))
                .is_some_and(|v| v.contains(&format!("\"{who}\"")))
        })
    };
    let before: Vec<_> = ["P0_0", "P0_1", "P1_0", "P1_1"]
        .iter()
        .map(|n| slot_of(&p, n).unwrap())
        .collect();
    assert_eq!(before, vec![0, 1, 2, 3]);

    // P0_1 leaves.
    let mut teams2 = teams.clone();
    teams2[0].remove(1);
    w.set_players(&mut mem, &teams2);
    let p = run(&mut roster, &mem).unwrap();
    assert_eq!(value(&p, "match_info", "roster_1"), None);
    assert_eq!(value(&p, "match_info", "items_1"), None);
    assert_eq!(slot_of(&p, "P0_0"), Some(0));
    assert_eq!(slot_of(&p, "P1_0"), Some(2));
    assert_eq!(slot_of(&p, "P1_1"), Some(3));

    // A newcomer takes the freed slot; the others still keep theirs.
    let mut teams3 = teams2.clone();
    teams3[0].push(Player::new("New", 99, 20));
    w.set_players(&mut mem, &teams3);
    let p = run(&mut roster, &mem).unwrap();
    assert_eq!(slot_of(&p, "New"), Some(1));
    assert_eq!(slot_of(&p, "P1_1"), Some(3));
}

#[test]
fn failed_pass_keeps_slot_assignments() {
    let (mut mem, w, mut tasks) = fresh();
    let teams = teams_of(&[2, 0]);
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    run(&mut roster, &mem).unwrap();
    // An invalid pass (two locals) must not change which slot an identity holds.
    let mut bad = teams.clone();
    bad[0][0].local = true;
    bad[0][1].local = true;
    w.set_players(&mut mem, &bad);
    assert!(run(&mut roster, &mem).is_err());
    let mut swapped = teams.clone();
    swapped[0].reverse();
    w.set_players(&mut mem, &swapped);
    let p = run(&mut roster, &mem).unwrap();
    assert!(
        value(&p, "match_info", "roster_0")
            .unwrap()
            .contains("P0_0")
    );
}

fn read_clock(clock: &mut Box<dyn Task>, mem: &FakeMem) -> Option<String> {
    run(clock, mem)
        .unwrap()
        .get("match_info", "match_clock")
        .unwrap()
        .map(String::from)
}

/// Expected values ported from underlock's tests/deadlock-clock.test.js.
#[test]
fn clock_matches_underlock_cases() {
    let g = world::CLOCK_GLOBALS;
    let rules = world::RULES_OBJ;

    // "game clock reproduces the client's pause-adjusted time".
    let (mut mem, _, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("08:15"));
    w32(&mut mem, rules + world::OFF_PAUSED_TICKS, 320); // 10 s previously paused
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("08:05"));
    w8(&mut mem, rules + world::OFF_PAUSED, 1);
    w32(&mut mem, rules + world::OFF_PAUSE_START, 16000);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("07:30"));
    wf(&mut mem, g + 0x30, 900.0);
    assert_eq!(
        read_clock(&mut clock, &mem).as_deref(),
        Some("07:30"),
        "pause does not advance"
    );
    w8(&mut mem, rules + world::OFF_PAUSED, 0);
    w32(&mut mem, rules + world::OFF_PAUSED_TICKS, 13120);
    assert_eq!(
        read_clock(&mut clock, &mem).as_deref(),
        Some("07:30"),
        "unpause subtracts paused ticks"
    );

    // "clock reads match offset and pregame settings and formats minute rollover".
    let (mut mem, _, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    wf(&mut mem, g + 0x30, 3999.375);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("66:00"));
    wf(&mut mem, world::OFFSET_CONVAR + 0x58, 5.0);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("66:05"));
    w8(&mut mem, world::PREGAME_ENABLED + 0x58, 1);
    w32(&mut mem, rules + world::OFF_MODE, 1);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("65:25"));
    wf(&mut mem, g + 0x30, 40.0);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("00:00"));
    // Before the match clock starts it never goes negative.
    wf(&mut mem, g + 0x30, 10.0);
    assert_eq!(read_clock(&mut clock, &mem).as_deref(), Some("00:00"));
}

#[test]
fn clock_runs_only_in_game_in_progress() {
    let (mut mem, w, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    assert!(
        run(&mut clock, &mem)
            .unwrap()
            .get("match_info", "match_clock")
            .unwrap()
            .is_some()
    );
    w.set_state(&mut mem, 8);
    let p = run(&mut clock, &mem).unwrap();
    assert_eq!(value(&p, "match_info", "match_clock"), None);
    assert_eq!(value(&p, "match_info", "game_paused"), None);
}

#[test]
fn clock_vtable_mismatch_is_invalid() {
    let (mut mem, _, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    world::w64(&mut mem, world::VTABLE + 0x220, 0x9999_9999);
    let mut p = Pass::default();
    let Err(TaskError::Invalid(why)) = clock.run(&mem, &mut p) else {
        panic!("expected Invalid");
    };
    assert!(why.contains("game-time"), "{why}");
    assert!(!staged(&p, "match_info", "match_clock"));
}

#[test]
fn clock_rejects_non_finite_and_out_of_range_state() {
    let (mut mem, w, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    let g = world::CLOCK_GLOBALS;
    wf(&mut mem, g + 0x30, f32::NAN);
    assert_eq!(run(&mut clock, &mem).err(), Some(TaskError::Torn));
    wf(&mut mem, g + 0x30, 535.0);
    wf(&mut mem, g + 0x54, 2.0);
    assert_eq!(run(&mut clock, &mem).err(), Some(TaskError::Torn));
    wf(&mut mem, g + 0x54, 0.03125);
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 2);
    assert_eq!(run(&mut clock, &mem).err(), Some(TaskError::Torn));
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 0);
    w32(
        &mut mem,
        world::RULES_OBJ + world::OFF_PAUSED_TICKS,
        (-1i32) as u32,
    );
    assert_eq!(run(&mut clock, &mem).err(), Some(TaskError::Torn));
    w32(&mut mem, world::RULES_OBJ + world::OFF_PAUSED_TICKS, 0);
    wf(&mut mem, g + 0x30, 1.0e9);
    assert_eq!(run(&mut clock, &mem).err(), Some(TaskError::Torn));
    wf(&mut mem, g + 0x30, 535.055_36);
    let _ = w;
    assert!(run(&mut clock, &mem).is_ok());
}

#[test]
fn clock_change_during_acquisition_is_discarded() {
    let (mem, _, mut tasks) = fresh();
    let globals = world::CLOCK_GLOBALS + 0x30;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        // The pause flag flips while the engine globals are being read.
        if addr == globals && len == 0x28 && !fired {
            fired = true;
            w8(mem, world::RULES_OBJ + world::OFF_PAUSED, 1);
        }
    });
    let mut clock = task(&mut tasks, "clock");
    let mut p = Pass::default();
    assert_eq!(clock.run(&hooked, &mut p), Err(TaskError::Torn));
    assert!(!staged(&p, "match_info", "match_clock"));
}

#[test]
fn clock_events_on_change_and_pause_edge() {
    let (mut mem, _, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    let events = |p: &Pass| -> Vec<(String, String)> { p.events().to_vec() };

    let p = run(&mut clock, &mem).unwrap();
    assert_eq!(
        events(&p),
        vec![("clock".into(), r#"{"match_clock":"08:15"}"#.into())]
    );
    assert_eq!(value(&p, "match_info", "game_paused"), Some("false"));

    // Same string again: no event.
    assert!(events(&run(&mut clock, &mem).unwrap()).is_empty());

    // Pausing at tick 16000 freezes the clock at 07:40 and emits clock, then game_paused.
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 1);
    w32(&mut mem, world::RULES_OBJ + world::OFF_PAUSE_START, 16000);
    let p = run(&mut clock, &mem).unwrap();
    assert_eq!(
        events(&p),
        vec![
            ("clock".into(), r#"{"match_clock":"07:40"}"#.into()),
            ("game_paused".into(), r#"{"game_paused":true}"#.into()),
        ]
    );
    assert_eq!(value(&p, "match_info", "game_paused"), Some("true"));
    // Still paused: nothing new.
    assert!(events(&run(&mut clock, &mem).unwrap()).is_empty());
}

#[test]
fn every_staged_key_is_in_the_schema_and_fits() {
    let (mut mem, w, tasks) = fresh();
    let mut teams = teams_of(&[6, 6]);
    for (n, p) in teams.iter_mut().flatten().enumerate() {
        p.items = (0..40).map(|i| 4_000_000_000 + i + n as u32).collect();
    }
    teams[0][0].local = true;
    w.set_players(&mut mem, &teams);
    let schema = deadlock_schema();
    for mut t in tasks {
        let mut p = Pass::default();
        t.run(&mem, &mut p).unwrap();
        for (cat, key, v) in p.sets() {
            let max = schema
                .keys()
                .find(|(c, k, _)| *c == cat.as_str() && *k == key.as_str())
                .unwrap_or_else(|| panic!("{cat}.{key} not in schema"))
                .2;
            assert!(
                v.as_ref().is_none_or(|v| v.len() as u32 <= max),
                "{cat}.{key}"
            );
        }
    }
}

#[test]
fn full_inventory_with_longest_name_fits_the_keys() {
    let (mut mem, w, mut tasks) = fresh();
    let mut teams = teams_of(&[1, 0]);
    teams[0][0].name = "a".repeat(127);
    teams[0][0].items = (0..64).map(|i| 4_000_000_000 + i).collect();
    w.set_players(&mut mem, &teams);
    let mut roster = task(&mut tasks, "roster");
    let p = run(&mut roster, &mem).unwrap();
    let schema = deadlock_schema();
    let max = |key: &str| {
        schema
            .keys()
            .find(|(_, k, _)| *k == key)
            .map(|x| x.2 as usize)
            .unwrap()
    };
    let items = value(&p, "match_info", "items_0").unwrap();
    assert!(items.len() > 1024, "{}", items.len());
    assert!(items.len() <= max("items_0"));
    assert!(value(&p, "match_info", "roster_0").unwrap().len() <= max("roster_0"));
}

#[test]
fn unresolved_fields_and_roots_list_the_task() {
    let (_mem, w) = World::standalone();
    // A schema without the roster classes.
    let mut s = Synth::new();
    let rules = s.class(
        "C_CitadelGameRules",
        &[("m_eGameState", 0x74), ("m_unMatchID", 0xa060)],
    );
    let table = s.class_table(&[rules]);
    s.classes_site(0x100, table);
    let (mem2, module) = s.finish();
    let f = uw_spike_dlcollector::schema::SchemaFields::load(&mem2, &module).unwrap();
    let built = build(
        &w.roots,
        Err(uw_spike_dlcollector::roots::RootError::NotFound(
            "game time globals",
        )),
        &f,
    )
    .unwrap();
    let names: Vec<_> = built.tasks.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["phase", "match_id"]);
    assert_eq!(built.unresolved.len(), 2);
    assert_eq!(built.unresolved[0].0, "roster");
    assert!(built.unresolved[0].1.starts_with("unresolved: "));
    assert_eq!(built.unresolved[1].0, "clock");
    assert_eq!(
        built.unresolved[1].1,
        "unresolved: game time globals not found"
    );
}

#[test]
fn build_rejects_missing_zero_or_huge_required_offsets() {
    let (_mem, w) = World::standalone();
    for off in [0u32, 0x10_0001] {
        let mut s = Synth::new();
        let rules = s.class(
            "C_CitadelGameRules",
            &[("m_eGameState", off), ("m_unMatchID", 0xa060)],
        );
        let table = s.class_table(&[rules]);
        s.classes_site(0x100, table);
        let (mem2, module) = s.finish();
        let f = uw_spike_dlcollector::schema::SchemaFields::load(&mem2, &module).unwrap();
        let err = build(&w.roots, Ok(w.clock.clone()), &f).err().unwrap();
        assert!(err.contains("m_eGameState"), "{err}");
    }
    let mut s = Synth::new();
    let rules = s.class("C_CitadelGameRules", &[("m_eGameState", 0x74)]);
    let table = s.class_table(&[rules]);
    s.classes_site(0x100, table);
    let (mem2, module) = s.finish();
    let f = uw_spike_dlcollector::schema::SchemaFields::load(&mem2, &module).unwrap();
    let err = build(&w.roots, Ok(w.clock.clone()), &f).err().unwrap();
    assert!(err.contains("m_unMatchID"), "{err}");
}
