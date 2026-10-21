//! `match_info.team_score`: each team's `C_Team.m_iScore`, in the shape Overwolf documents,
//! `{"amber":"6","sapphire":"0"}`. The game shows this number as the team's kills: on build
//! 6810 the HUD sets its `team_kills` dialog variable from the team's score getter
//! (`mov eax,[rcx+0x620]; ret` at client.dll+0xb256a0, its only call at +0x1c6f6ce).

use super::read::{VecHeader, ensure, pointer, read_bytes};
use super::{MAX_TEAMS, TEAM_AMBER, TEAM_SAPPHIRE, at, rules_object};
use crate::keys::MATCH_INFO;
use crate::mem::MemRead;
use crate::roots::Roots;
use crate::runner::{Pass, Task, TaskError};

/// The JSON object of the Amber and Sapphire scores among `teams` (`(team number, score)`); a
/// team that is not in the list is left out, and with neither there is no value.
fn format_scores(teams: &[(u8, i32)]) -> Result<Option<String>, TaskError> {
    let mut fields = Vec::new();
    for (key, number) in [("amber", TEAM_AMBER), ("sapphire", TEAM_SAPPHIRE)] {
        let mut scores = teams.iter().filter(|&&(n, _)| n == number);
        match (scores.next(), scores.next()) {
            (Some(_), Some(_)) => {
                return Err(TaskError::Invalid(format!(
                    "more than one team with number {number}"
                )));
            }
            (Some((_, score)), None) => fields.push(format!("\"{key}\":\"{score}\"")),
            _ => {}
        }
    }
    Ok((!fields.is_empty()).then(|| format!("{{{}}}", fields.join(","))))
}

pub struct TeamScore {
    teams: u64,
    rules_ptr: u64,
    team_num: u32,
    score: u32,
}

impl TeamScore {
    pub fn new(roots: &Roots, team_num: u32, score: u32) -> TeamScore {
        TeamScore {
            teams: roots.teams,
            rules_ptr: roots.game_rules_ptr,
            team_num,
            score,
        }
    }

    fn team(&self, m: &dyn MemRead, team: u64) -> Result<(u8, i32), TaskError> {
        Ok((
            m.read_u8(at(team, self.team_num.into())?)?,
            m.read_i32(at(team, self.score.into())?)?,
        ))
    }

    /// `(team number, score)` of every team in the game's team list.
    fn read_teams(&self, m: &dyn MemRead) -> Result<Vec<(u8, i32)>, TaskError> {
        let list = VecHeader::read(m, self.teams, MAX_TEAMS)?;
        let raw = list.elements(m, 8)?;
        let mut teams = Vec::new();
        for team_raw in raw.chunks_exact(8) {
            if let Some(team) = pointer(team_raw)? {
                teams.push((team, self.team(m, team)?));
            }
        }
        // Every team again, then the list: values that changed meanwhile are not one state.
        for &(team, read) in &teams {
            ensure(self.team(m, team)? == read)?;
        }
        list.unchanged(m)?;
        ensure(list.count == 0 || read_bytes(m, list.data, raw.len())? == raw)?;
        Ok(teams.into_iter().map(|(_, read)| read).collect())
    }
}

impl Task for TeamScore {
    fn name(&self) -> &'static str {
        "team_score"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let value = match rules_object(m, self.rules_ptr)? {
            None => None,
            Some(_) => format_scores(&self.read_teams(m)?)?,
        };
        out.set(MATCH_INFO, "team_score", value);
        Ok(())
    }
}
