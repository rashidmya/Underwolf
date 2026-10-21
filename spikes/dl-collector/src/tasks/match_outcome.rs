//! `match_info.match_outcome`: the winning team, in the shape Overwolf documents,
//! `{"winning_team":"SAPPHIRE"}` (values `AMBER` and `SAPPHIRE`).
//!
//! Read the way the game's own match-end panel reads it on build 6810 (client.dll+0x1c14110):
//! `C_CitadelGameRules.m_iWinningTeam` (an int, rules +0xc0, from the same game-rules global),
//! only in `PostGame` and `End`, with 2 shown as `Team1Victory` and 3 as `Team2Victory`. In
//! `Abandoned` the panel shows `MatchAbandoned` and no winner.

use super::phase::{END, POST_GAME};
use super::{TEAM_AMBER, TEAM_SAPPHIRE, at, rules_object};
use crate::keys::MATCH_INFO;
use crate::mem::MemRead;
use crate::runner::{Pass, Task, TaskError};

/// The phases in which the game shows the winner.
const SHOWN_IN: [i32; 2] = [POST_GAME, END];

/// Overwolf's name of the winner `team` in `state`; `None` outside `SHOWN_IN` and for a team
/// number that is not Amber or Sapphire.
fn winner(state: i32, team: i32) -> Option<&'static str> {
    if !SHOWN_IN.contains(&state) {
        return None;
    }
    match u8::try_from(team) {
        Ok(TEAM_AMBER) => Some("AMBER"),
        Ok(TEAM_SAPPHIRE) => Some("SAPPHIRE"),
        _ => None,
    }
}

pub struct MatchOutcome {
    rules_ptr: u64,
    state: u32,
    winner: u32,
}

impl MatchOutcome {
    pub fn new(rules_ptr: u64, state: u32, winner: u32) -> MatchOutcome {
        MatchOutcome {
            rules_ptr,
            state,
            winner,
        }
    }

    fn read(&self, m: &dyn MemRead, rules: u64) -> Result<(i32, i32), TaskError> {
        Ok((
            m.read_i32(at(rules, self.state.into())?)?,
            m.read_i32(at(rules, self.winner.into())?)?,
        ))
    }
}

impl Task for MatchOutcome {
    fn name(&self) -> &'static str {
        "match_outcome"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let value = match rules_object(m, self.rules_ptr)? {
            None => None,
            Some(rules) => {
                // Two separate reads: a pair that changed in between is not one state.
                let (state, team) = self.read(m, rules)?;
                if self.read(m, rules)? != (state, team) {
                    return Err(TaskError::Torn);
                }
                winner(state, team).map(|w| format!("{{\"winning_team\":\"{w}\"}}"))
            }
        };
        out.set(MATCH_INFO, "match_outcome", value);
        Ok(())
    }
}
