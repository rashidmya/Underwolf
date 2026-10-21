//! `uw-gep verify`: compares the final roster a `uw-gep watch` run wrote to JSONL with the match
//! metadata deadlock-api.com publishes after the match (per player: hero, team, kills, deaths,
//! assists; plus the match id). The comparison itself (`compare`) is pure; `run` fetches the
//! metadata with `curl` and prints the table.
//!
//! The game's roster rows carry `steam_id` (SteamID64) and the game's team number; the API has
//! `account_id` (the low 32 bits of the SteamID64) and team 0/1. Players are matched on the
//! account id. Team numbers differ between the two sources, so teams are compared through the
//! one-to-one mapping that agrees with most players (printed in the report).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::Value;

const STEAM64_BASE: u64 = 76_561_197_960_265_728;
const FIELDS: [&str; 5] = ["hero", "team", "kills", "deaths", "assists"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Mismatch,
    Missing,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "OK",
            Verdict::Mismatch => "MISMATCH",
            Verdict::Missing => "MISSING",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Account id, or `-` for the match id row.
    pub player: String,
    pub field: &'static str,
    /// What the collector published (`None`: nothing).
    pub game: Option<String>,
    /// What the API says (`None`: the API has no such player).
    pub api: Option<String>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
    /// `(game team, API team)` pairs used for the team comparison.
    pub team_map: Vec<(u64, u64)>,
}

impl Report {
    pub fn count(&self, v: Verdict) -> usize {
        self.rows.iter().filter(|r| r.verdict == v).count()
    }

    pub fn render(&self) -> String {
        let cell = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        let mut out = format!("{:<12} {:<8} {:>12} {:>12}  {}\n", "player", "field", "game", "api", "result");
        for r in &self.rows {
            let _ = writeln!(
                out,
                "{:<12} {:<8} {:>12} {:>12}  {}",
                r.player,
                r.field,
                cell(&r.game),
                cell(&r.api),
                r.verdict.label()
            );
        }
        let map: Vec<String> = self.team_map.iter().map(|(g, a)| format!("game {g} = api {a}")).collect();
        let _ = writeln!(
            out,
            "team mapping: {}",
            if map.is_empty() { "none".to_string() } else { map.join(", ") }
        );
        let _ = writeln!(
            out,
            "{} OK, {} MISMATCH, {} MISSING",
            self.count(Verdict::Ok),
            self.count(Verdict::Mismatch),
            self.count(Verdict::Missing)
        );
        out
    }
}

/// 0: everything OK. 1: at least one MISMATCH. 3: no MISMATCH, but something was not published.
pub fn exit_code(r: &Report) -> i32 {
    if r.count(Verdict::Mismatch) > 0 {
        1
    } else if r.count(Verdict::Missing) > 0 {
        3
    } else {
        0
    }
}

/// A player's last roster row published while the requested match was current.
struct GameRow {
    hero: Option<u64>,
    team: Option<u64>,
    counts: [Option<u64>; 3],
}

fn account_id(steam_id: &str) -> String {
    match steam_id.parse::<u64>().ok().and_then(|s| s.checked_sub(STEAM64_BASE)) {
        Some(a) if a <= u64::from(u32::MAX) => a.to_string(),
        _ => steam_id.to_string(),
    }
}

/// What `uw-gep watch` wrote for `match_id`: whether its id appeared, and each player's last roster
/// row seen while that id was the current `match_info.match_id`.
fn read_log(jsonl: &str, match_id: u64) -> (bool, BTreeMap<String, GameRow>) {
    let wanted = match_id.to_string();
    let mut current: Option<String> = None;
    let mut seen = false;
    let mut rows = BTreeMap::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["kind"] != "info" || v["category"] != "match_info" {
            continue;
        }
        let (Some(key), value) = (v["key"].as_str(), v["value"].as_str()) else { continue };
        if key == "match_id" {
            current = value.map(Into::into);
            seen |= current.as_deref() == Some(wanted.as_str());
        } else if key.starts_with("roster_") && current.as_deref() == Some(wanted.as_str()) {
            let Some(row) = value.and_then(|s| serde_json::from_str::<Value>(s).ok()) else { continue };
            let Some(steam_id) = row["steam_id"].as_str() else { continue };
            let n = |k: &str| row[k].as_u64();
            rows.insert(
                account_id(steam_id),
                GameRow { hero: n("hero_id"), team: n("team_id"), counts: [n("kills"), n("deaths"), n("assists")] },
            );
        }
    }
    (seen, rows)
}

/// The one-to-one game team → API team mapping that agrees with the most players (ties: lower
/// ids first).
fn team_mapping(pairs: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut counts: BTreeMap<(u64, u64), usize> = BTreeMap::new();
    for &p in pairs {
        *counts.entry(p).or_default() += 1;
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by(|(pa, ca), (pb, cb)| cb.cmp(ca).then(pa.cmp(pb)));
    let (mut game_used, mut api_used) = (BTreeSet::new(), BTreeSet::new());
    let mut map = Vec::new();
    for ((g, a), _) in ranked {
        if game_used.insert(g) {
            if api_used.insert(a) {
                map.push((g, a));
            } else {
                game_used.remove(&g);
            }
        }
    }
    map.sort();
    map
}

fn verdict(game: &Option<String>, api: &Option<String>) -> Verdict {
    match (game, api) {
        (None, _) => Verdict::Missing,
        (Some(g), Some(a)) if g == a => Verdict::Ok,
        _ => Verdict::Mismatch,
    }
}

/// Compares a `uw-gep watch` JSONL log with the deadlock-api.com metadata JSON of `match_id`.
pub fn compare(jsonl: &str, metadata: &str, match_id: u64) -> Result<Report, String> {
    let meta: Value = serde_json::from_str(metadata).map_err(|e| format!("metadata is not JSON: {e}"))?;
    let info = &meta["match_info"];
    let api_id = info["match_id"].as_u64().ok_or("metadata has no match_info.match_id")?;
    if api_id != match_id {
        return Err(format!("metadata is for match {api_id}, not {match_id}"));
    }
    let api_players = info["players"].as_array().filter(|p| !p.is_empty()).ok_or("metadata has no players")?;
    let (id_seen, game) = read_log(jsonl, match_id);

    struct Api {
        account: String,
        team: Option<u64>,
        values: [Option<String>; 5],
    }
    let api: Vec<Api> = api_players
        .iter()
        .filter_map(|p| {
            let value = |k: &str| p[k].as_u64().map(|n| n.to_string());
            Some(Api {
                account: p["account_id"].as_u64()?.to_string(),
                team: p["team"].as_u64(),
                values: [value("hero_id"), None, value("kills"), value("deaths"), value("assists")],
            })
        })
        .collect();

    let pairs: Vec<(u64, u64)> = api
        .iter()
        .filter_map(|p| Some((game.get(&p.account)?.team?, p.team?)))
        .collect();
    let team_map = team_mapping(&pairs);

    let mut rows = vec![Row {
        player: "-".into(),
        field: "match_id",
        game: id_seen.then(|| match_id.to_string()),
        api: Some(match_id.to_string()),
        verdict: if id_seen { Verdict::Ok } else { Verdict::Missing },
    }];
    for p in &api {
        let g = game.get(&p.account);
        for (i, field) in FIELDS.into_iter().enumerate() {
            let (game_value, api_value) = match field {
                "team" => {
                    let game_team = g.and_then(|g| g.team);
                    // A game team with no mapping disagrees with every API team.
                    let mapped = game_team.and_then(|t| team_map.iter().find(|(g, _)| *g == t)).map(|(_, a)| *a);
                    let verdict = match game_team {
                        None => Verdict::Missing,
                        Some(_) if mapped.is_some() && mapped == p.team => Verdict::Ok,
                        Some(_) => Verdict::Mismatch,
                    };
                    rows.push(Row {
                        player: p.account.clone(),
                        field,
                        game: game_team.map(|t| t.to_string()),
                        api: p.team.map(|t| t.to_string()),
                        verdict,
                    });
                    continue;
                }
                "hero" => (g.and_then(|g| g.hero), p.values[i].clone()),
                _ => (g.and_then(|g| g.counts[i - 2]), p.values[i].clone()),
            };
            let game_value = game_value.map(|n| n.to_string());
            rows.push(Row {
                player: p.account.clone(),
                field,
                verdict: verdict(&game_value, &api_value),
                game: game_value,
                api: api_value,
            });
        }
    }
    // Published for the match, but not a player of it.
    let known: BTreeSet<&str> = api.iter().map(|p| p.account.as_str()).collect();
    for (account, g) in &game {
        if !known.contains(account.as_str()) {
            rows.push(Row {
                player: account.clone(),
                field: "player",
                game: Some(g.hero.map_or("?".into(), |h| format!("hero {h}"))),
                api: None,
                verdict: Verdict::Mismatch,
            });
        }
    }
    Ok(Report { rows, team_map })
}

fn fetch(match_id: u64) -> Result<String, String> {
    let url = format!("https://api.deadlock-api.com/v1/matches/{match_id}/metadata");
    let out = std::process::Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location", "--max-time", "60", &url])
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("fetching {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{url}: {e}"))
}

fn usage() -> String {
    "usage: uw-gep verify --jsonl out.jsonl --match-id <id> [--metadata file.json]".into()
}

/// `uw-gep verify`; `args` excludes the program name and the subcommand. Returns the exit code.
pub fn run(args: impl IntoIterator<Item = String>) -> Result<i32, String> {
    let mut args = args.into_iter();
    let (mut jsonl, mut id, mut metadata): (Option<PathBuf>, Option<u64>, Option<PathBuf>) = (None, None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--jsonl" => jsonl = Some(value()?.into()),
            "--match-id" => id = Some(value()?.parse().map_err(|e| format!("--match-id: {e}"))?),
            "--metadata" => metadata = Some(value()?.into()),
            _ => return Err(format!("unknown argument {flag}\n{}", usage())),
        }
    }
    let (jsonl, id) = (jsonl.ok_or_else(usage)?, id.ok_or_else(usage)?);
    let log = std::fs::read_to_string(&jsonl).map_err(|e| format!("{}: {e}", jsonl.display()))?;
    let meta = match metadata {
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => fetch(id)?,
    };
    let report = compare(&log, &meta, id)?;
    print!("{}", report.render());
    Ok(exit_code(&report))
}
