use uw_spike_gep_host::provider::{Emission, Provider};

fn row(local: bool, steam_id: &str, kills: u32, deaths: u32, assists: u32) -> String {
    format!(
        r#"{{"player_name":"p{steam_id}","steam_id":"{steam_id}","hero_id":7,"team_id":2,"is_local":{local},"kills":{kills},"deaths":{deaths},"assists":{assists}}}"#
    )
}

fn events(out: &[Emission]) -> Vec<String> {
    out.iter()
        .filter_map(|e| match e {
            Emission::Event { name, .. } => Some(name.clone()),
            Emission::Info { .. } => None,
        })
        .collect()
}

/// Feeds a sequence of `(category, key, value)` and collects the event names.
fn feed(p: &mut Provider, infos: &[(&str, &str, Option<&str>)]) -> Vec<String> {
    infos.iter().flat_map(|&(c, k, v)| events(&p.on_info(c, k, v))).collect()
}

fn local(p: &mut Provider, k: u32, d: u32, a: u32) -> Vec<String> {
    events(&p.on_info("match_info", "roster_3", Some(&row(true, "76561198000000001", k, d, a))))
}

#[test]
fn info_passes_through_with_feature_equal_to_category() {
    let mut p = Provider::default();
    assert_eq!(
        p.on_info("game_info", "game_mode", Some("bots")),
        [Emission::Info {
            feature: "game_info".into(),
            category: "game_info".into(),
            key: "game_mode".into(),
            value: Some("bots".into()),
        }]
    );
    assert_eq!(
        p.on_info("match_info", "team_score", None),
        [Emission::Info {
            feature: "match_info".into(),
            category: "match_info".into(),
            key: "team_score".into(),
            value: None,
        }]
    );
}

#[test]
fn kill_delta_emits_kill_event() {
    let mut p = Provider::default();
    p.on_info("match_info", "match_id", Some("100"));
    assert!(local(&mut p, 0, 0, 0).is_empty(), "the first sighting is a baseline");
    assert_eq!(local(&mut p, 1, 0, 0), ["kill"]);
    assert!(local(&mut p, 1, 0, 0).is_empty());
    assert_eq!(local(&mut p, 2, 0, 0), ["kill"]);
    // Another player's kills are not ours.
    let other = row(false, "76561198000000002", 9, 0, 0);
    assert!(events(&p.on_info("match_info", "roster_4", Some(&other))).is_empty());
    let other = row(false, "76561198000000002", 10, 0, 0);
    assert!(events(&p.on_info("match_info", "roster_4", Some(&other))).is_empty());
}

#[test]
fn death_and_assist_deltas_emit_events() {
    let mut p = Provider::default();
    p.on_info("match_info", "match_id", Some("100"));
    local(&mut p, 3, 1, 4);
    assert_eq!(local(&mut p, 3, 2, 4), ["death"]);
    assert_eq!(local(&mut p, 3, 2, 5), ["assist"]);
    assert_eq!(local(&mut p, 4, 2, 6), ["kill", "assist"]);
}

#[test]
fn new_match_resets_counters_without_events() {
    let mut p = Provider::default();
    p.on_info("match_info", "match_id", Some("100"));
    local(&mut p, 5, 2, 1);
    assert!(feed(&mut p, &[("match_info", "match_id", Some("200"))]).is_empty());
    assert!(local(&mut p, 0, 0, 0).is_empty());
    assert_eq!(local(&mut p, 1, 0, 0), ["kill"]);
    // A cleared match id is a different scope too.
    assert!(feed(&mut p, &[("match_info", "match_id", None)]).is_empty());
    assert!(local(&mut p, 2, 0, 0).is_empty());
}

#[test]
fn match_start_once_per_match() {
    let mut p = Provider::default();
    let start = [("match_info", "match_id", Some("100")), ("game_info", "phase", Some("GameInProgress"))];
    assert_eq!(feed(&mut p, &start), ["match_start"]);
    assert!(feed(&mut p, &[("game_info", "phase", Some("GameInProgress"))]).is_empty());
    assert_eq!(feed(&mut p, &[("game_info", "phase", Some("End"))]), ["match_end"]);
    // Back in progress in the same match: no second start.
    assert!(feed(&mut p, &[("game_info", "phase", Some("GameInProgress"))]).is_empty());
    assert_eq!(feed(&mut p, &[("match_info", "match_id", Some("200"))]), ["match_start"]);
}

#[test]
fn phase_transition_emits_match_start_and_end() {
    let mut p = Provider::default();
    // In one transaction the helper relays `phase` (game_info) before `match_id` (match_info).
    let seq = [
        ("game_info", "phase", Some("PreGameWait")),
        ("game_info", "phase", Some("GameInProgress")),
        ("match_info", "match_id", Some("100")),
        ("game_info", "phase", Some("PostGame")),
        ("game_info", "phase", Some("PostGame_PlayOfTheGame")),
        ("game_info", "phase", Some("End")),
        ("game_info", "phase", Some("Invalid")),
    ];
    assert_eq!(feed(&mut p, &seq), ["match_start", "match_end"]);

    // A cleared phase (match unloaded) after a start ends the match.
    let seq = [
        ("match_info", "match_id", Some("300")),
        ("game_info", "phase", Some("GameInProgress")),
        ("game_info", "phase", None),
    ];
    assert_eq!(feed(&mut p, &seq), ["match_start", "match_end"]);

    // Leaving a phase without a start emits nothing.
    let mut p = Provider::default();
    assert!(feed(&mut p, &[("game_info", "phase", Some("PostGame")), ("game_info", "phase", Some("End"))]).is_empty());
}

#[test]
fn clock_event_becomes_match_clock() {
    let mut p = Provider::default();
    let data = r#"{"match_clock":"01:02"}"#;
    assert_eq!(p.on_event("clock", data), [Emission::Event { name: "match_clock".into(), data: data.into() }]);
    // Other events pass through unchanged, `game_paused` with Overwolf's bare boolean.
    for (name, data) in [("game_paused", "true"), ("game_paused", "false"), ("tower_destroyed", "{}")] {
        assert_eq!(p.on_event(name, data), [Emission::Event { name: name.into(), data: data.into() }]);
    }
}

#[test]
fn local_steam_id_comes_from_local_roster_row() {
    let mut p = Provider::default();
    let other = row(false, "76561198000000002", 0, 0, 0);
    let out = p.on_info("match_info", "roster_0", Some(&other));
    assert_eq!(out.len(), 1, "a non-local row is only passed through: {out:?}");

    let mine = row(true, "76561198000000001", 0, 0, 0);
    let out = p.on_info("match_info", "roster_3", Some(&mine));
    assert_eq!(
        out,
        [
            Emission::Info {
                feature: "match_info".into(),
                category: "match_info".into(),
                key: "roster_3".into(),
                value: Some(mine.clone()),
            },
            Emission::Info {
                feature: "game_info".into(),
                category: "game_info".into(),
                key: "steam_id".into(),
                value: Some("76561198000000001".into()),
            },
        ]
    );
    // Unchanged: not repeated.
    let mine = row(true, "76561198000000001", 1, 0, 0);
    assert!(
        !p.on_info("match_info", "roster_3", Some(&mine))
            .iter()
            .any(|e| matches!(e, Emission::Info { key, .. } if key == "steam_id"))
    );
}
