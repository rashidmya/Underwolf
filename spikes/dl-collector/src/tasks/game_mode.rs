//! `game_info.game_mode`: the game rules' match mode and game mode, in the shape Overwolf
//! documents, `{"match_mode":"Unranked","game_mode":"Normal"}`.

use super::{at, rules_object};
use crate::keys::GAME_INFO;
use crate::mem::MemRead;
use crate::runner::{Pass, Task, TaskError};

/// `ECitadelMatchMode` names by value, without the `k_ECitadelMatchMode_` prefix (Overwolf's
/// handler strips it). Build 6810's schema enum and protobuf descriptor in client.dll both
/// list exactly these; `tests/installed_build.rs` checks the schema enum on the installed build.
pub const MATCH_MODES: [&str; 9] = [
    "Invalid",
    "Unranked",
    "PrivateLobby",
    "CoopBot",
    "Ranked",
    "ServerTest",
    "Tutorial",
    "HeroLabs",
    "NewPlayerPlacement",
];

/// `ECitadelGameMode` names by value, without the `k_ECitadelGameMode_` prefix (as above).
pub const GAME_MODES: [&str; 7] = [
    "Invalid",
    "Normal",
    "1v1Test",
    "Sandbox",
    "StreetBrawl",
    "ExploreNYC",
    "Internal",
];

/// The name of `v` in `table`. `None` for 0 (`Invalid`: not set) and for numbers the table
/// does not name.
fn name(table: &[&'static str], v: i32) -> Option<&'static str> {
    match usize::try_from(v) {
        Ok(i) if i > 0 => table.get(i).copied(),
        _ => None,
    }
}

/// The JSON object of the named modes; a mode without a name is left out, and with neither
/// there is no value.
fn format_modes(match_mode: i32, game_mode: i32) -> Option<String> {
    let fields: Vec<String> = [
        ("match_mode", name(&MATCH_MODES, match_mode)),
        ("game_mode", name(&GAME_MODES, game_mode)),
    ]
    .into_iter()
    .filter_map(|(key, v)| v.map(|v| format!("\"{key}\":\"{v}\"")))
    .collect();
    (!fields.is_empty()).then(|| format!("{{{}}}", fields.join(",")))
}

pub struct GameMode {
    rules_ptr: u64,
    match_mode: u32,
    game_mode: u32,
}

impl GameMode {
    pub fn new(rules_ptr: u64, match_mode: u32, game_mode: u32) -> GameMode {
        GameMode {
            rules_ptr,
            match_mode,
            game_mode,
        }
    }

    fn modes(&self, m: &dyn MemRead, rules: u64) -> Result<(i32, i32), TaskError> {
        Ok((
            m.read_i32(at(rules, self.match_mode.into())?)?,
            m.read_i32(at(rules, self.game_mode.into())?)?,
        ))
    }
}

impl Task for GameMode {
    fn name(&self) -> &'static str {
        "game_mode"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let value = match rules_object(m, self.rules_ptr)? {
            None => None,
            Some(rules) => {
                // Two separate reads: a pair that changed in between is not one state.
                let (match_mode, game_mode) = self.modes(m, rules)?;
                if self.modes(m, rules)? != (match_mode, game_mode) {
                    return Err(TaskError::Torn);
                }
                format_modes(match_mode, game_mode)
            }
        };
        out.set(GAME_INFO, "game_mode", value);
        Ok(())
    }
}
