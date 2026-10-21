mod common;

use common::world::{self, Player, World, teams_of, w8, w32, w64, wf};
use common::{Hooked, Synth};
use uw_spike_dlcollector::keys::deadlock_schema;
use uw_spike_dlcollector::mem::{FakeMem, MemRead, ReadError};
use uw_spike_dlcollector::runner::{Pass, Runner, Task, TaskError};
use uw_spike_dlcollector::schema::SchemaFields;
use uw_spike_dlcollector::tasks::{TASK_FIELDS, build};

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
    set_modes(&mut mem, 3, 1);
    w.set_winner(&mut mem, 2);
    let names = [
        "phase",
        "match_id",
        "clock",
        "roster",
        "game_mode",
        "team_score",
        "match_outcome",
    ];
    for name in names {
        let mut t = task(&mut tasks, name);
        // The winner is shown after the match; the clock only during it.
        w.set_state(&mut mem, if name == "match_outcome" { 8 } else { 7 });
        let p = run(&mut t, &mem).unwrap();
        // A valid pass first: the keys carry values.
        let key = match name {
            "phase" => ("game_info", "phase"),
            "match_id" => ("match_info", "match_id"),
            "clock" => ("match_info", "match_clock"),
            "game_mode" => ("game_info", "game_mode"),
            "team_score" => ("match_info", "team_score"),
            "match_outcome" => ("match_info", "match_outcome"),
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
    // Overwolf's event data is the bare boolean: `{"name":"game_paused","data":true}`.
    assert_eq!(
        events(&p),
        vec![
            ("clock".into(), r#"{"match_clock":"07:40"}"#.into()),
            ("game_paused".into(), "true".into()),
        ]
    );
    assert_eq!(value(&p, "match_info", "game_paused"), Some("true"));
    // Still paused: nothing new.
    assert!(events(&run(&mut clock, &mem).unwrap()).is_empty());

    // Unpausing (the paused ticks now include the pause) keeps 07:40 and emits `false`.
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 0);
    w32(&mut mem, world::RULES_OBJ + world::OFF_PAUSED_TICKS, 1120);
    let p = run(&mut clock, &mem).unwrap();
    assert_eq!(events(&p), vec![("game_paused".into(), "false".into())]);
    assert_eq!(value(&p, "match_info", "game_paused"), Some("false"));
    assert!(events(&run(&mut clock, &mem).unwrap()).is_empty());
}

#[test]
fn game_paused_event_needs_a_seen_transition() {
    // Paused from the first pass: the info key says so, but no change was seen, so no event.
    let (mut mem, w, mut tasks) = fresh();
    let mut clock = task(&mut tasks, "clock");
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 1);
    w32(&mut mem, world::RULES_OBJ + world::OFF_PAUSE_START, 16000);
    let p = run(&mut clock, &mem).unwrap();
    assert!(!p.events().iter().any(|(n, _)| n == "game_paused"));
    assert_eq!(value(&p, "match_info", "game_paused"), Some("true"));

    // Leaving GameInProgress clears the key and forgets the state: unpausing outside a match
    // and coming back unpaused emits nothing.
    w.set_state(&mut mem, 8);
    assert_eq!(
        value(&run(&mut clock, &mem).unwrap(), "match_info", "game_paused"),
        None
    );
    w8(&mut mem, world::RULES_OBJ + world::OFF_PAUSED, 0);
    w.set_state(&mut mem, 7);
    let p = run(&mut clock, &mem).unwrap();
    assert!(!p.events().iter().any(|(n, _)| n == "game_paused"));
    assert_eq!(value(&p, "match_info", "game_paused"), Some("false"));

    // A torn pass in between does not hide a transition.
    let mut fired = false;
    let globals = world::CLOCK_GLOBALS + 0x30;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        if addr == globals && len == 0x28 && !fired {
            fired = true;
            w8(mem, world::RULES_OBJ + world::OFF_PAUSED, 1);
        }
    });
    assert_eq!(
        clock.run(&hooked, &mut Pass::default()),
        Err(TaskError::Torn)
    );
    let p = run(&mut clock, &hooked).unwrap();
    assert_eq!(
        p.events()
            .iter()
            .filter(|(n, _)| n == "game_paused")
            .collect::<Vec<_>>(),
        [&("game_paused".to_string(), "true".to_string())]
    );
}

fn set_modes(mem: &mut FakeMem, match_mode: i32, game_mode: i32) {
    w32(
        mem,
        world::RULES_OBJ + world::OFF_MATCH_MODE,
        match_mode as u32,
    );
    w32(mem, world::RULES_OBJ + world::OFF_MODE, game_mode as u32);
}

fn read_mode(t: &mut Box<dyn Task>, mem: &dyn MemRead) -> Option<String> {
    value(&run(t, mem).unwrap(), "game_info", "game_mode").map(String::from)
}

/// Overwolf documents `{"match_mode":"Unranked","game_mode":"Normal"}`: the game's
/// `ECitadelMatchMode` / `ECitadelGameMode` names without their `k_ECitadel…_` prefix.
#[test]
fn game_mode_publishes_both_enum_names_in_overwolfs_shape() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "game_mode");
    set_modes(&mut mem, 1, 1);
    assert_eq!(
        read_mode(&mut t, &mem).as_deref(),
        Some(r#"{"match_mode":"Unranked","game_mode":"Normal"}"#)
    );
    // Every named value of build 6810, by number.
    let match_modes = [
        "Unranked",
        "PrivateLobby",
        "CoopBot",
        "Ranked",
        "ServerTest",
        "Tutorial",
        "HeroLabs",
        "NewPlayerPlacement",
    ];
    for (i, name) in match_modes.iter().enumerate() {
        set_modes(&mut mem, i as i32 + 1, 3);
        assert_eq!(
            read_mode(&mut t, &mem),
            Some(format!(
                r#"{{"match_mode":"{name}","game_mode":"Sandbox"}}"#
            ))
        );
    }
    let game_modes = [
        "Normal",
        "1v1Test",
        "Sandbox",
        "StreetBrawl",
        "ExploreNYC",
        "Internal",
    ];
    for (i, name) in game_modes.iter().enumerate() {
        set_modes(&mut mem, 3, i as i32 + 1);
        assert_eq!(
            read_mode(&mut t, &mem),
            Some(format!(
                r#"{{"match_mode":"CoopBot","game_mode":"{name}"}}"#
            ))
        );
    }
    // Not tied to GameInProgress: published whenever the game rules exist.
    w.set_state(&mut mem, 3);
    assert!(read_mode(&mut t, &mem).is_some());
    w.rules_present(&mut mem, false);
    assert_eq!(read_mode(&mut t, &mem), None);
}

/// `Invalid` (0) means not set and a number without a name is unknown: neither is emitted.
#[test]
fn game_mode_leaves_out_invalid_and_unknown_values() {
    let (mut mem, _, mut tasks) = fresh();
    let mut t = task(&mut tasks, "game_mode");
    for bad in [0, 9, -1, i32::MAX] {
        set_modes(&mut mem, bad, 1);
        assert_eq!(
            read_mode(&mut t, &mem).as_deref(),
            Some(r#"{"game_mode":"Normal"}"#),
            "{bad}"
        );
    }
    for bad in [0, 7, -1, i32::MIN] {
        set_modes(&mut mem, 2, bad);
        assert_eq!(
            read_mode(&mut t, &mem).as_deref(),
            Some(r#"{"match_mode":"PrivateLobby"}"#),
            "{bad}"
        );
    }
    set_modes(&mut mem, 0, 0);
    assert_eq!(read_mode(&mut t, &mem), None);
    set_modes(&mut mem, 100, -5);
    assert_eq!(read_mode(&mut t, &mem), None);
}

#[test]
fn game_mode_change_during_read_is_discarded() {
    let (mut mem, _, mut tasks) = fresh();
    set_modes(&mut mem, 3, 1);
    let game_mode = world::RULES_OBJ + world::OFF_MODE;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        // The match mode changes right after the first read of the game mode.
        if addr == game_mode && len == 4 && !fired {
            fired = true;
            w32(mem, world::RULES_OBJ + world::OFF_MATCH_MODE, 1);
        }
    });
    let mut t = task(&mut tasks, "game_mode");
    let mut p = Pass::default();
    assert_eq!(t.run(&hooked, &mut p), Err(TaskError::Torn));
    assert!(!staged(&p, "game_info", "game_mode"));
    assert_eq!(
        read_mode(&mut t, &hooked).as_deref(),
        Some(r#"{"match_mode":"Unranked","game_mode":"Normal"}"#)
    );
}

fn read_scores(t: &mut Box<dyn Task>, mem: &dyn MemRead) -> Option<String> {
    value(&run(t, mem).unwrap(), "match_info", "team_score").map(String::from)
}

/// Overwolf documents `{"amber": "6" , "sapphire" : "0"}`: each team's score as a string under
/// its team's name, team 2 being Amber and team 3 Sapphire.
#[test]
fn team_score_publishes_amber_and_sapphire_in_overwolfs_shape() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "team_score");
    // `set_players` numbers the teams 2, 3, 4 (neutral, not emitted).
    w.set_players(&mut mem, &teams_of(&[1, 1, 0]));
    w.set_score(&mut mem, 0, 6);
    w.set_score(&mut mem, 2, 99);
    assert_eq!(
        read_scores(&mut t, &mem).as_deref(),
        Some(r#"{"amber":"6","sapphire":"0"}"#)
    );
    // The game's int as it is, extremes included.
    w.set_score(&mut mem, 0, i32::MIN);
    w.set_score(&mut mem, 1, i32::MAX);
    assert_eq!(
        read_scores(&mut t, &mem).as_deref(),
        Some(r#"{"amber":"-2147483648","sapphire":"2147483647"}"#)
    );
    // Names follow the team number, not the list order.
    w8(&mut mem, World::team(0) + world::OFF_TEAM_ID, 3);
    w8(&mut mem, World::team(1) + world::OFF_TEAM_ID, 2);
    assert_eq!(
        read_scores(&mut t, &mem).as_deref(),
        Some(r#"{"amber":"2147483647","sapphire":"-2147483648"}"#)
    );
    // Not tied to a phase: published whenever the game rules exist.
    w.set_state(&mut mem, 3);
    assert!(read_scores(&mut t, &mem).is_some());
    w.rules_present(&mut mem, false);
    assert_eq!(read_scores(&mut t, &mem), None);
}

/// Unassigned (0), spectator (1), neutral (4) and numbers the game does not name are not
/// Overwolf teams; a team missing from the list is left out, and with neither there is no value.
#[test]
fn team_score_leaves_out_other_and_missing_teams() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "team_score");
    w.set_players(&mut mem, &teams_of(&[0, 0]));
    w.set_score(&mut mem, 0, 5);
    w.set_score(&mut mem, 1, 7);
    for other in [0u8, 1, 4, 5, 255] {
        w8(&mut mem, World::team(1) + world::OFF_TEAM_ID, other);
        assert_eq!(
            read_scores(&mut t, &mem).as_deref(),
            Some(r#"{"amber":"5"}"#),
            "{other}"
        );
    }
    w8(&mut mem, World::team(1) + world::OFF_TEAM_ID, 3);
    // A null slot in the team list is skipped.
    w64(&mut mem, world::TEAM_ARRAY, 0);
    assert_eq!(
        read_scores(&mut t, &mem).as_deref(),
        Some(r#"{"sapphire":"7"}"#)
    );
    w8(&mut mem, World::team(1) + world::OFF_TEAM_ID, 1);
    assert_eq!(read_scores(&mut t, &mem), None);
    w.set_players(&mut mem, &[]);
    assert_eq!(read_scores(&mut t, &mem), None);
}

#[test]
fn team_score_with_two_teams_of_one_number_is_invalid() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "team_score");
    w.set_players(&mut mem, &teams_of(&[1, 1]));
    w8(&mut mem, World::team(1) + world::OFF_TEAM_ID, 2);
    assert!(matches!(run(&mut t, &mem), Err(TaskError::Invalid(_))));
}

#[test]
fn team_score_change_during_read_is_discarded() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[1, 1, 0]));
    let sapphire = World::team(1) + world::OFF_SCORE;
    let mut reads = 0;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        if addr != sapphire || len != 4 {
            return;
        }
        reads += 1;
        match reads {
            // Amber scores right after Sapphire's score was read.
            1 => w32(mem, World::team(0) + world::OFF_SCORE, 1),
            // Once both scores were read again: a team is swapped in the list.
            3 => w64(mem, world::TEAM_ARRAY + 16, World::team(1)),
            _ => {}
        }
    });
    let mut t = task(&mut tasks, "team_score");
    for _ in 0..2 {
        let mut p = Pass::default();
        assert_eq!(t.run(&hooked, &mut p), Err(TaskError::Torn));
        assert!(!staged(&p, "match_info", "team_score"));
    }
    // Now the list holds Sapphire twice.
    assert!(matches!(run(&mut t, &hooked), Err(TaskError::Invalid(_))));
}

fn read_outcome(t: &mut Box<dyn Task>, mem: &dyn MemRead) -> Option<String> {
    value(&run(t, mem).unwrap(), "match_info", "match_outcome").map(String::from)
}

/// Overwolf documents `{"winning_team":"SAPPHIRE"}` with the values AMBER and SAPPHIRE. The
/// game's match-end panel shows the winner in PostGame and End.
#[test]
fn match_outcome_publishes_the_winner_after_the_match() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "match_outcome");
    w.set_winner(&mut mem, 3);
    for state in [8, 11] {
        w.set_state(&mut mem, state);
        assert_eq!(
            read_outcome(&mut t, &mem).as_deref(),
            Some(r#"{"winning_team":"SAPPHIRE"}"#),
            "{state}"
        );
    }
    w.set_winner(&mut mem, 2);
    assert_eq!(
        read_outcome(&mut t, &mem).as_deref(),
        Some(r#"{"winning_team":"AMBER"}"#)
    );
    w.rules_present(&mut mem, false);
    assert_eq!(read_outcome(&mut t, &mem), None);
}

/// No value in any other phase (in Abandoned the game shows `MatchAbandoned`), and none for a
/// team number other than Amber's or Sapphire's.
#[test]
fn match_outcome_needs_a_shown_phase_and_a_named_team() {
    let (mut mem, w, mut tasks) = fresh();
    let mut t = task(&mut tasks, "match_outcome");
    w.set_winner(&mut mem, 2);
    for state in [0, 1, 2, 3, 4, 5, 6, 7, 9, 10, 12, -1, i32::MAX] {
        w.set_state(&mut mem, state);
        assert_eq!(read_outcome(&mut t, &mem), None, "{state}");
    }
    w.set_state(&mut mem, 8);
    for team in [0, 1, 4, 5, -1, 0x102, 0x103, i32::MAX, i32::MIN] {
        w.set_winner(&mut mem, team);
        assert_eq!(read_outcome(&mut t, &mem), None, "{team}");
    }
}

#[test]
fn match_outcome_change_during_read_is_discarded() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_state(&mut mem, 8);
    w.set_winner(&mut mem, 3);
    let winner = world::RULES_OBJ + world::OFF_WINNER;
    let mut fired = false;
    let hooked = Hooked::new(mem, move |addr, len, mem| {
        // The winner changes right after its first read.
        if addr == winner && len == 4 && !fired {
            fired = true;
            w32(mem, winner, 2);
        }
    });
    let mut t = task(&mut tasks, "match_outcome");
    let mut p = Pass::default();
    assert_eq!(t.run(&hooked, &mut p), Err(TaskError::Torn));
    assert!(!staged(&p, "match_info", "match_outcome"));
    assert_eq!(
        read_outcome(&mut t, &hooked).as_deref(),
        Some(r#"{"winning_team":"AMBER"}"#)
    );
}

#[test]
fn team_score_and_match_outcome_fit_their_keys() {
    let (mut mem, w, mut tasks) = fresh();
    w.set_players(&mut mem, &teams_of(&[0, 0]));
    w.set_score(&mut mem, 0, i32::MIN);
    w.set_score(&mut mem, 1, i32::MIN);
    w.set_state(&mut mem, 8);
    w.set_winner(&mut mem, 3);
    let schema = deadlock_schema();
    for key in ["team_score", "match_outcome"] {
        let mut t = task(&mut tasks, key);
        let p = run(&mut t, &mem).unwrap();
        let v = value(&p, "match_info", key).unwrap();
        let max = schema
            .keys()
            .find(|(c, k, _)| *c == "match_info" && *k == key)
            .unwrap()
            .2;
        assert!(v.len() as u32 <= max, "{key}: {v}");
    }
}

#[test]
fn every_staged_key_is_in_the_schema_and_fits() {
    let (mut mem, w, tasks) = fresh();
    // The longest game_mode value.
    set_modes(&mut mem, 8, 4);
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
    assert_eq!(built.unresolved.len(), 5);
    assert_eq!(built.unresolved[0].0, "roster");
    assert!(built.unresolved[0].1.starts_with("unresolved: "));
    assert_eq!(built.unresolved[1].0, "clock");
    assert_eq!(
        built.unresolved[1].1,
        "unresolved: game time globals not found"
    );
    assert_eq!(
        built.unresolved[2],
        (
            "game_mode",
            "unresolved: C_CitadelGameRules.m_eMatchMode".to_string()
        )
    );
    assert_eq!(
        built.unresolved[3],
        (
            "team_score",
            "unresolved: C_BaseEntity.m_iTeamNum".to_string()
        )
    );
    assert_eq!(
        built.unresolved[4],
        (
            "match_outcome",
            "unresolved: C_CitadelGameRules.m_iWinningTeam".to_string()
        )
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

/// Every `(address, length)` a run reads.
struct Reads<'a> {
    mem: &'a FakeMem,
    log: std::cell::RefCell<Vec<(u64, usize)>>,
}

impl MemRead for Reads<'_> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        self.log.borrow_mut().push((addr, buf.len()));
        self.mem.read(addr, buf)
    }
}

/// The reads of one run of each task built from `f`, by task name.
fn reads_per_task(
    mem: &FakeMem,
    w: &World,
    f: &SchemaFields,
) -> std::collections::BTreeMap<&'static str, Vec<(u64, usize)>> {
    let built = build(&w.roots, Ok(w.clock.clone()), f).unwrap();
    built
        .tasks
        .into_iter()
        .map(|mut t| {
            let r = Reads {
                mem,
                log: Default::default(),
            };
            let _ = t.run(&r, &mut Pass::default());
            (t.name(), r.log.into_inner())
        })
        .collect()
}

/// A task whose reads change when a field moves must list that field in `TASK_FIELDS`, even
/// when `build` hands it an offset resolved once for several tasks (`m_eGameState`).
#[test]
fn task_fields_list_exactly_the_tasks_whose_reads_follow_each_field() {
    let (mut mem, w) = World::standalone();
    w.set_players(&mut mem, &teams_of(&[2, 2]));
    let base = reads_per_task(&mem, &w, &world::fields());
    for &(_, class, field) in TASK_FIELDS {
        let mut f = world::fields();
        assert!(f.add_offset(class, field, 8), "{class}.{field}");
        let moved = reads_per_task(&mem, &w, &f);
        let readers: std::collections::BTreeSet<_> = base
            .iter()
            .filter(|&(task, reads)| &moved[task] != reads)
            .map(|(task, _)| *task)
            .collect();
        let listed: std::collections::BTreeSet<_> = TASK_FIELDS
            .iter()
            .filter(|row| (row.1, row.2) == (class, field))
            .map(|row| row.0)
            .collect();
        assert_eq!(readers, listed, "{class}.{field}");
    }
}
