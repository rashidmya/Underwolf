use serde_json::{Value, json};
use uw_spike_gep_host::verify::{Report, Verdict, compare, exit_code};

const FIXTURE: &str = include_str!("fixtures/match-metadata-116950735.json");
const MATCH_ID: u64 = 116950735;
const STEAM64_BASE: u64 = 76561197960265728;

#[derive(Clone)]
struct P {
    account_id: u64,
    team: u64,
    hero: u64,
    kills: u64,
    deaths: u64,
    assists: u64,
}

/// The fixture's players. In-game team ids are 2 and 3 (API: 0 and 1).
fn players() -> Vec<P> {
    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    v["match_info"]["players"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let n = |k: &str| p[k].as_u64().unwrap();
            P {
                account_id: n("account_id"),
                team: n("team") + 2,
                hero: n("hero_id"),
                kills: n("kills"),
                deaths: n("deaths"),
                assists: n("assists"),
            }
        })
        .collect()
}

fn info(category: &str, key: &str, value: Option<&str>) -> String {
    json!({"t_ms": 1, "kind": "info", "feature": category, "category": category, "key": key, "value": value, "ts": 1})
        .to_string()
}

fn match_id(id: &str) -> String {
    info("match_info", "match_id", Some(id))
}

fn roster(slot: usize, p: &P) -> String {
    let row = json!({
        "player_name": format!("p{}", p.account_id),
        "steam_id": (STEAM64_BASE + p.account_id).to_string(),
        "hero_id": p.hero,
        "team_id": p.team,
        "is_local": p.account_id == 1595847997,
        "kills": p.kills,
        "deaths": p.deaths,
        "assists": p.assists,
    });
    info("match_info", &format!("roster_{slot}"), Some(&row.to_string()))
}

fn session(id: &str, ps: &[P]) -> Vec<String> {
    let mut lines = vec![info("game_info", "phase", Some("GameInProgress")), match_id(id)];
    lines.extend(ps.iter().enumerate().map(|(i, p)| roster(i, p)));
    lines
}

fn run(lines: &[String]) -> Report {
    compare(&lines.join("\n"), FIXTURE, MATCH_ID).unwrap()
}

fn rows<'a>(r: &'a Report, field: &str, verdict: Verdict) -> Vec<&'a str> {
    r.rows.iter().filter(|x| x.field == field && x.verdict == verdict).map(|x| x.player.as_str()).collect()
}

#[test]
fn fixture_has_twelve_players() {
    assert_eq!(players().len(), 12);
}

#[test]
fn matching_roster_is_all_ok() {
    let r = run(&session("116950735", &players()));
    assert_eq!(r.team_map, [(2, 0), (3, 1)]);
    // match id + 12 players x (hero, team, kills, deaths, assists)
    assert_eq!(r.rows.len(), 1 + 12 * 5);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
    assert_eq!(exit_code(&r), 0);
}

#[test]
fn wrong_kills_is_one_mismatch() {
    let mut ps = players();
    ps[3].kills += 1;
    let r = run(&session("116950735", &ps));
    let bad: Vec<_> = r.rows.iter().filter(|x| x.verdict != Verdict::Ok).collect();
    assert_eq!(bad.len(), 1, "{}", r.render());
    assert_eq!(bad[0].field, "kills");
    assert_eq!(bad[0].verdict, Verdict::Mismatch);
    assert_eq!(bad[0].player, ps[3].account_id.to_string());
    assert_eq!(bad[0].game.as_deref(), Some((ps[3].kills).to_string().as_str()));
    assert_eq!(bad[0].api.as_deref(), Some((ps[3].kills - 1).to_string().as_str()));
    assert_eq!(exit_code(&r), 1);
}

#[test]
fn wrong_hero_and_deaths_assists_are_mismatches() {
    let mut ps = players();
    ps[0].hero += 1;
    ps[1].deaths += 2;
    ps[2].assists += 1;
    let r = run(&session("116950735", &ps));
    assert_eq!(rows(&r, "hero", Verdict::Mismatch).len(), 1);
    assert_eq!(rows(&r, "deaths", Verdict::Mismatch).len(), 1);
    assert_eq!(rows(&r, "assists", Verdict::Mismatch).len(), 1);
    assert_eq!(rows(&r, "kills", Verdict::Mismatch).len(), 0);
}

#[test]
fn player_never_published_is_missing_in_every_field() {
    let ps = players();
    let r = run(&session("116950735", &ps[1..]));
    let missing: Vec<_> = r.rows.iter().filter(|x| x.verdict == Verdict::Missing).collect();
    assert_eq!(missing.len(), 5, "{}", r.render());
    assert!(missing.iter().all(|x| x.player == ps[0].account_id.to_string() && x.game.is_none()));
    assert_eq!(exit_code(&r), 3);
}

#[test]
fn mismatch_wins_over_missing_in_exit_code() {
    let mut ps = players();
    ps[5].kills += 1;
    let r = run(&session("116950735", &ps[1..]));
    assert_eq!(exit_code(&r), 1);
}

#[test]
fn final_value_is_the_last_row_and_a_cleared_slot_does_not_erase_it() {
    let ps = players();
    let mut early = ps.clone();
    for p in &mut early {
        p.kills = 0;
        p.deaths = 0;
        p.assists = 0;
    }
    let mut lines = session("116950735", &early);
    lines.extend(ps.iter().enumerate().map(|(i, p)| roster(i, p)));
    lines.extend((0..12).map(|i| info("match_info", &format!("roster_{i}"), None)));
    let r = run(&lines);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
}

#[test]
fn slot_reuse_by_another_player_keeps_each_players_last_row() {
    let ps = players();
    let mut lines = vec![match_id("116950735")];
    // Everyone sits in slot 0 in turn: rows are told apart by steam id, not by slot.
    lines.extend(ps.iter().map(|p| roster(0, p)));
    let r = run(&lines);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
}

#[test]
fn rows_of_other_matches_are_ignored() {
    let ps = players();
    let mut wrong = ps.clone();
    for p in &mut wrong {
        p.kills += 7;
    }
    let mut lines = session("999", &wrong);
    lines.extend(session("116950735", &ps));
    lines.extend(session("1000", &wrong));
    let r = run(&lines);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
}

#[test]
fn match_id_never_seen_is_missing_and_every_player_is_missing() {
    let r = run(&session("999", &players()));
    assert_eq!(rows(&r, "match_id", Verdict::Missing), ["-"]);
    assert_eq!(r.rows.iter().filter(|x| x.verdict == Verdict::Missing).count(), 1 + 12 * 5);
}

#[test]
fn empty_log_is_all_missing() {
    let r = run(&[]);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Missing));
    assert_eq!(exit_code(&r), 3);
}

#[test]
fn player_not_in_the_api_match_is_a_mismatch() {
    let mut ps = players();
    let mut stranger = ps[0].clone();
    stranger.account_id = 42;
    ps.push(stranger);
    let r = run(&session("116950735", &ps));
    let bad: Vec<_> = r.rows.iter().filter(|x| x.verdict == Verdict::Mismatch).collect();
    assert_eq!(bad.len(), 1, "{}", r.render());
    assert_eq!((bad[0].player.as_str(), bad[0].field), ("42", "player"));
    assert_eq!(bad[0].api, None);
}

#[test]
fn team_ids_are_compared_as_a_consistent_mapping() {
    // 2/3 in game vs 0/1 in the API is fine; so is the reverse assignment.
    let mut ps = players();
    for p in &mut ps {
        p.team = if p.team == 2 { 3 } else { 2 };
    }
    let r = run(&session("116950735", &ps));
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
    assert_eq!(r.team_map, [(2, 1), (3, 0)]);
}

#[test]
fn a_player_on_the_wrong_team_is_the_only_team_mismatch() {
    let mut ps = players();
    ps[4].team = if ps[4].team == 2 { 3 } else { 2 };
    let r = run(&session("116950735", &ps));
    let bad = rows(&r, "team", Verdict::Mismatch);
    assert_eq!(bad, [ps[4].account_id.to_string()]);
    assert_eq!(r.rows.iter().filter(|x| x.verdict != Verdict::Ok).count(), 1);
}

#[test]
fn unrelated_and_garbled_lines_are_ignored() {
    let mut lines = vec![
        "not json".to_string(),
        json!({"t_ms": 1, "kind": "status", "status": "connected"}).to_string(),
        json!({"t_ms": 1, "kind": "event", "name": "kill", "data": "{}", "ts": 1}).to_string(),
        info("match_info", "roster_0", Some("not json either")),
        String::new(),
    ];
    lines.extend(session("116950735", &players()));
    let r = run(&lines);
    assert!(r.rows.iter().all(|x| x.verdict == Verdict::Ok), "{}", r.render());
}

#[test]
fn metadata_for_another_match_is_an_error() {
    let err = compare("", FIXTURE, 5).unwrap_err();
    assert!(err.contains("116950735") && err.contains('5'), "{err}");
}

#[test]
fn metadata_without_players_is_an_error() {
    assert!(compare("", r#"{"match_info":{"match_id":5}}"#, 5).is_err());
    assert!(compare("", "nope", 5).is_err());
}

#[test]
fn render_lists_every_row_with_its_verdict() {
    let mut ps = players();
    ps[0].kills += 1;
    let r = run(&session("116950735", &ps[..11]));
    let text = r.render();
    assert!(text.contains("OK") && text.contains("MISMATCH") && text.contains("MISSING"));
    assert!(text.contains(&ps[0].account_id.to_string()));
    assert!(text.contains("team mapping"));
}
