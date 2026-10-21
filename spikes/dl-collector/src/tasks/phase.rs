use super::{at, rules_object};
use crate::keys::GAME_INFO;
use crate::mem::MemRead;
use crate::runner::{Pass, Task, TaskError};

const PHASES: [&str; 12] = [
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

pub(super) const GAME_IN_PROGRESS: i32 = 7;
pub(super) const POST_GAME: i32 = 8;
pub(super) const END: i32 = 11;

/// The game's phase name for `m_eGameState`; anything outside the table is `Invalid`.
fn phase_name(state: i32) -> &'static str {
    usize::try_from(state)
        .ok()
        .and_then(|i| PHASES.get(i))
        .copied()
        .unwrap_or("Invalid")
}

pub struct Phase {
    rules_ptr: u64,
    state: u32,
}

impl Phase {
    pub fn new(rules_ptr: u64, state: u32) -> Phase {
        Phase { rules_ptr, state }
    }
}

impl Task for Phase {
    fn name(&self) -> &'static str {
        "phase"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let phase = match rules_object(m, self.rules_ptr)? {
            None => None,
            Some(rules) => Some(phase_name(m.read_i32(at(rules, self.state.into())?)?).to_string()),
        };
        out.set(GAME_INFO, "phase", phase);
        Ok(())
    }
}
