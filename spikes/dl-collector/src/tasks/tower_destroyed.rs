//! The `tower_destroyed` event: one per boss objective an Overwolf team loses during a Normal
//! match in `GameInProgress`, with that team's name as data. Overwolf documents
//! `{"name":"tower_destroyed","data":"Amber"}` with the values `Amber` and `Sapphire`.
//!
//! Source: each team's `C_CitadelTeam.m_nBossesAlive`, the field Overwolf's plugin compares from
//! pass to pass (gep_deadlock.dll 314.1.0, +0x1407c0: it fires with the team's name when the
//! value changes). On build 6810 its network change notifier (server.dll+0x897f20) has two
//! callers:
//! - server.dll+0x86c180 adds 1 (and 1 to `m_nBossesMax`) when an entity joins the team and
//!   its class is `CLASS_TROOPER_BOSS`, `CLASS_DESTROYABLE_BUILDING`, `CLASS_BOSS_TIER_2`,
//!   `CLASS_BOSS_BARRACKS` or `CLASS_BOSS_TIER_3`.
//! - server.dll+0x8a7950 takes 1 off for every boss passed to it, whether or not its handle
//!   was in the team's list. Only virtuals at slots 14 and 105 call it; for the boss classes
//!   these are `ChangeTeam` (slot 105) and slot 14, read as the entity's removal (not proven
//!   offline; the live check settles it, and that each boss is removed once).
//!
//! A drop between two passes of one match is therefore that many of the team's boss objectives
//! gone. A rise only moves the baseline. The first pass of a match has nothing to compare
//! with. Only Normal matches count (see `counted`).

use super::phase::GAME_IN_PROGRESS;
use super::read::read_team_list;
use super::{GAME_MODES, TEAM_AMBER, TEAM_SAPPHIRE, at, rules_object};
use crate::mem::MemRead;
use crate::roots::Roots;
use crate::runner::{Pass, Task, TaskError};

/// Overwolf's team names (its documented event values), by the game's team number.
const TEAMS: [(u8, &str); 2] = [(TEAM_AMBER, "Amber"), (TEAM_SAPPHIRE, "Sapphire")];

/// More bosses than this on one team are not believed (a bounded count, like `MAX_TEAMS`).
pub const MAX_BOSSES: i32 = 64;

/// A team as read: its number and, for Amber and Sapphire, `(bosses alive, bosses max)`.
type TeamRead = (u8, Option<(i32, i32)>);

/// Per entry of `TEAMS`: the team object and its bosses alive.
type Bosses = [Option<(u64, i32)>; 2];

/// The bosses alive of each Overwolf team among `teams`. A team that is not in the list is
/// `None`. Two teams with one number, or counts outside `0 <= alive <= max <= MAX_BOSSES`, are
/// not a state of the game.
fn bosses(teams: &[(u64, TeamRead)]) -> Result<Bosses, TaskError> {
    let mut out = [None; 2];
    for (slot, &(number, _)) in out.iter_mut().zip(&TEAMS) {
        let mut found = teams.iter().filter(|(_, (n, _))| *n == number);
        match (found.next(), found.next()) {
            (Some(_), Some(_)) => {
                return Err(TaskError::Invalid(format!(
                    "more than one team with number {number}"
                )));
            }
            (Some(&(team, (_, Some((alive, max))))), None) => {
                if !(0 <= alive && alive <= max && max <= MAX_BOSSES) {
                    return Err(TaskError::Invalid(format!(
                        "team {number}: {alive} of {max} bosses alive"
                    )));
                }
                *slot = Some((team, alive));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The schema offsets the task reads: game rules `m_eGameState`, `m_eGameMode` and
/// `m_unMatchID`; the team's `m_iTeamNum`, `m_nBossesAlive` and `m_nBossesMax`.
pub struct TowerOffsets {
    pub state: u32,
    pub game_mode: u32,
    pub match_id: u32,
    pub team_num: u32,
    pub alive: u32,
    pub max: u32,
}

/// The match a baseline belongs to: the game rules object and its match id.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Match {
    rules: u64,
    id: u32,
}

pub struct TowerDestroyed {
    teams: u64,
    rules_ptr: u64,
    at: TowerOffsets,
    /// The bosses alive at the last good pass, and the match they were read in.
    last: Option<(Match, Bosses)>,
}

impl TowerDestroyed {
    pub fn new(roots: &Roots, at: TowerOffsets) -> TowerDestroyed {
        TowerDestroyed {
            teams: roots.teams,
            rules_ptr: roots.game_rules_ptr,
            at,
            last: None,
        }
    }

    /// `(phase, game mode, match id)` of the game rules object `rules`.
    fn rules(&self, m: &dyn MemRead, rules: u64) -> Result<(i32, i32, u32), TaskError> {
        Ok((
            m.read_i32(at(rules, self.at.state.into())?)?,
            m.read_i32(at(rules, self.at.game_mode.into())?)?,
            m.read_u32(at(rules, self.at.match_id.into())?)?,
        ))
    }

    fn team(&self, m: &dyn MemRead, team: u64) -> Result<TeamRead, TaskError> {
        let number = m.read_u8(at(team, self.at.team_num.into())?)?;
        if !TEAMS.iter().any(|&(n, _)| n == number) {
            return Ok((number, None));
        }
        let alive = m.read_i32(at(team, self.at.alive.into())?)?;
        let max = m.read_i32(at(team, self.at.max.into())?)?;
        Ok((number, Some((alive, max))))
    }
}

/// Only a Normal match in progress: the game mode is the schema enum's `Normal`
/// (`GAME_MODES`, checked against the installed build's `ECitadelGameMode`). Other modes
/// (Sandbox in the Hideout, StreetBrawl with its rounds) were not analysed.
fn counted(state: i32, mode: i32) -> bool {
    state == GAME_IN_PROGRESS
        && usize::try_from(mode).ok().and_then(|i| GAME_MODES.get(i)) == Some(&"Normal")
}

impl Task for TowerDestroyed {
    fn name(&self) -> &'static str {
        "tower_destroyed"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let Some(rules) = rules_object(m, self.rules_ptr)? else {
            self.last = None;
            return Ok(());
        };
        let (state, mode, id) = self.rules(m, rules)?;
        if !counted(state, mode) {
            self.last = None;
            return Ok(());
        }
        let teams = read_team_list(m, self.teams, |m, team| self.team(m, team))?;
        // The match again: counts read as it ends or changes are not one state of it.
        if rules_object(m, self.rules_ptr)? != Some(rules)
            || self.rules(m, rules)? != (state, mode, id)
        {
            return Err(TaskError::Torn);
        }
        let now = bosses(&teams)?;
        let here = Match { rules, id };
        if let Some((there, was)) = &self.last
            && *there == here
        {
            for ((&(_, name), was), is) in TEAMS.iter().zip(was).zip(&now) {
                // Only within one team object: a new object starts its own count.
                if let (Some((team_was, alive_was)), Some((team, alive))) = (was, is)
                    && team_was == team
                {
                    for _ in *alive..*alive_was {
                        out.event("tower_destroyed", format!("\"{name}\""));
                    }
                }
            }
        }
        self.last = Some((here, now));
        Ok(())
    }
}
