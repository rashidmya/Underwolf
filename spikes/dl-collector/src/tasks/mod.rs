//! The collection tasks and the resolution of what each one needs.

mod clock;
mod game_mode;
mod match_id;
mod match_outcome;
mod phase;
mod roster;
mod team_score;

use crate::mem::{MemRead, PTR_MAX, PTR_MIN, ReadError};
use crate::roots::{CLOCK_RULE_FIELDS, ClockRoots, RootError, Roots};
use crate::runner::{Task, TaskError};
use crate::schema::FieldLookup;

pub use clock::Clock;
pub use game_mode::{GAME_MODES, GameMode, MATCH_MODES};
pub use match_id::MatchId;
pub use match_outcome::MatchOutcome;
pub use phase::Phase;
pub use roster::{Roster, RosterFields};
pub use team_score::TeamScore;

/// Field offsets of 0 or beyond this are not believed (a broken schema read, not a field).
const FIELD_OFFSET_MAX: u32 = 1 << 20;

const RULES: &str = "C_CitadelGameRules";

/// The game's team numbers of Overwolf's two teams. Build 6810's client.dll names 2 "Amber"
/// (`#Citadel_Team1_Name`) and 3 "Sapphire" (`#Citadel_Team2_Name`) in its team table, and its
/// parser turns `TEAM_AMBER` into 2 and `TEAM_SAPPHIRE` into 3 (`tests/installed_build.rs`
/// checks the parser). Other numbers (0 unassigned, 1 spectator, 4 neutral) are not emitted.
pub const TEAM_AMBER: u8 = 2;
pub const TEAM_SAPPHIRE: u8 = 3;

/// Every schema field each task reads, as `(task, class, field)`. The layout check
/// (`crate::layout`) switches a task off when one of its fields moved. Tests hold this list to
/// exactly the fields `build` looks up, and each row to the tasks whose reads follow that field:
/// an offset `build` hands to several tasks (like `m_eGameState`) needs a row for each.
pub const TASK_FIELDS: &[(&str, &str, &str)] = &[
    ("phase", RULES, "m_eGameState"),
    ("match_id", RULES, "m_unMatchID"),
    ("clock", RULES, "m_eGameState"),
    ("clock", "C_GameRules", "m_nTotalPausedTicks"),
    ("clock", "C_GameRules", "m_nPauseStartTick"),
    ("clock", "C_GameRules", "m_bGamePaused"),
    ("clock", RULES, "m_flGameStartTime"),
    ("clock", RULES, "m_eGameMode"),
    ("roster", "C_Team", "m_aPlayerControllers"),
    ("roster", "C_BaseEntity", "m_iTeamNum"),
    ("roster", "CBasePlayerController", "m_iszPlayerName"),
    ("roster", "CBasePlayerController", "m_steamID"),
    (
        "roster",
        "CBasePlayerController",
        "m_bIsLocalPlayerController",
    ),
    ("roster", "CCitadelPlayerController", "m_PlayerDataGlobal"),
    ("roster", "PlayerDataGlobal_t", "m_nHeroID"),
    ("roster", "PlayerDataGlobal_t", "m_iPlayerKills"),
    ("roster", "PlayerDataGlobal_t", "m_iDeaths"),
    ("roster", "PlayerDataGlobal_t", "m_iPlayerAssists"),
    ("roster", "PlayerDataGlobal_t", "m_vecUpgrades"),
    ("game_mode", RULES, "m_eMatchMode"),
    ("game_mode", RULES, "m_eGameMode"),
    ("team_score", "C_BaseEntity", "m_iTeamNum"),
    ("team_score", "C_Team", "m_iScore"),
    ("match_outcome", RULES, "m_eGameState"),
    ("match_outcome", RULES, "m_iWinningTeam"),
];

pub struct Built {
    pub tasks: Vec<Box<dyn Task>>,
    /// Tasks that were not created, with `"unresolved: <what>"`.
    pub unresolved: Vec<(&'static str, String)>,
}

fn offset(f: &dyn FieldLookup, class: &str, field: &str) -> Option<u32> {
    f.get(class, field)
        .filter(|&o| o != 0 && o <= FIELD_OFFSET_MAX)
}

fn required(f: &dyn FieldLookup, class: &str, field: &str) -> Result<u32, String> {
    offset(f, class, field).ok_or_else(|| format!("required field {class}.{field} not found"))
}

/// Creates every task whose roots and fields resolved. `Err` carries the reason a required
/// field (`m_eGameState`, `m_unMatchID`) is missing; any other gap only leaves its task out.
pub fn build(
    roots: &Roots,
    clock: Result<ClockRoots, RootError>,
    fields: &dyn FieldLookup,
) -> Result<Built, String> {
    let state = required(fields, RULES, "m_eGameState")?;
    let match_id = required(fields, RULES, "m_unMatchID")?;
    let mut tasks: Vec<Box<dyn Task>> = vec![
        Box::new(Phase::new(roots.game_rules_ptr, state)),
        Box::new(MatchId::new(roots.game_rules_ptr, match_id)),
    ];
    let mut unresolved = Vec::new();

    match RosterFields::resolve(fields) {
        Ok(rf) => tasks.push(Box::new(Roster::new(roots, rf))),
        Err(what) => unresolved.push(("roster", format!("unresolved: {what}"))),
    }

    let clock_fields = CLOCK_RULE_FIELDS
        .iter()
        .map(|&(_, class, field)| offset(fields, class, field).ok_or(format!("{class}.{field}")))
        .collect::<Result<Vec<_>, _>>();
    match (clock, clock_fields) {
        (Err(e), _) => unresolved.push(("clock", format!("unresolved: {e}"))),
        (_, Err(what)) => unresolved.push(("clock", format!("unresolved: {what}"))),
        (Ok(roots_c), Ok(f)) => tasks.push(Box::new(Clock::new(
            roots.game_rules_ptr,
            roots_c,
            state,
            [f[0], f[1], f[2], f[3], f[4]],
        ))),
    }

    match (
        offset(fields, RULES, "m_eMatchMode"),
        offset(fields, RULES, "m_eGameMode"),
    ) {
        (Some(match_mode), Some(game_mode)) => tasks.push(Box::new(GameMode::new(
            roots.game_rules_ptr,
            match_mode,
            game_mode,
        ))),
        (None, _) => unresolved.push(("game_mode", format!("unresolved: {RULES}.m_eMatchMode"))),
        (_, None) => unresolved.push(("game_mode", format!("unresolved: {RULES}.m_eGameMode"))),
    }

    match (
        offset(fields, "C_BaseEntity", "m_iTeamNum"),
        offset(fields, "C_Team", "m_iScore"),
    ) {
        (Some(team_num), Some(score)) => {
            tasks.push(Box::new(TeamScore::new(roots, team_num, score)));
        }
        (None, _) => unresolved.push(("team_score", "unresolved: C_BaseEntity.m_iTeamNum".into())),
        (_, None) => unresolved.push(("team_score", "unresolved: C_Team.m_iScore".into())),
    }

    match offset(fields, RULES, "m_iWinningTeam") {
        Some(winner) => tasks.push(Box::new(MatchOutcome::new(
            roots.game_rules_ptr,
            state,
            winner,
        ))),
        None => unresolved.push((
            "match_outcome",
            format!("unresolved: {RULES}.m_iWinningTeam"),
        )),
    }
    Ok(Built { tasks, unresolved })
}

/// The game rules object: `None` while the global is null (no match loaded).
fn rules_object(m: &dyn MemRead, global: u64) -> Result<Option<u64>, TaskError> {
    match m.read_u64(global)? {
        0 => Ok(None),
        p if (PTR_MIN..=PTR_MAX).contains(&p) => Ok(Some(p)),
        p => Err(ReadError::Implausible(p).into()),
    }
}

/// `addr + off`, refusing a sum that leaves the plausible pointer range.
fn at(addr: u64, off: u64) -> Result<u64, TaskError> {
    match addr.checked_add(off) {
        Some(a) if a <= PTR_MAX => Ok(a),
        _ => Err(ReadError::Implausible(addr).into()),
    }
}

/// A JSON string literal for `s`.
pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
