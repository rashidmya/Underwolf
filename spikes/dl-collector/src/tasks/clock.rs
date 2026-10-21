//! The match clock: the arithmetic of the client's own formatter (as recovered in underlock's
//! `readDeadlockClock`), over roots found by byte pattern.

use super::phase::GAME_IN_PROGRESS;
use super::{at, rules_object};
use crate::keys::MATCH_INFO;
use crate::mem::MemRead;
use crate::roots::ClockRoots;
use crate::runner::{Pass, Task, TaskError};

/// The game rules' vtable slot of the game-time adjust function.
const VTABLE_ADJUST_SLOT: u64 = 0x220;
/// Engine globals read in one block: time (f32) at +0, tick interval (f32) at +0x24.
const GLOBALS_BLOCK_AT: u64 = 0x30;
const GLOBALS_BLOCK_LEN: usize = 0x28;
const GLOBALS_INTERVAL_AT: usize = 0x24;
/// ConVar value, relative to the ConVar data pointer.
const CONVAR_VALUE: u64 = 0x58;

const MIN_SECONDS: f64 = -3600.0;
const MAX_SECONDS: f64 = 86400.0;

#[derive(PartialEq)]
struct Controls {
    paused_ticks: i32,
    pause_start: i32,
    paused: u8,
    start: u32,
    mode: i32,
    offset: u32,
    pregame: u32,
    enabled: u8,
}

struct Pointers {
    globals: u64,
    vtable: u64,
    offset: u64,
    pregame: u64,
    enabled: u64,
}

pub struct Clock {
    rules_ptr: u64,
    roots: ClockRoots,
    state: u32,
    /// Offsets of total paused ticks, pause start tick, paused flag, game start time, game mode.
    fields: [u32; 5],
    last_clock: Option<String>,
    last_paused: Option<bool>,
}

/// Rounds to the nearest f32, as the client's single-precision arithmetic does.
fn f32r(x: f64) -> f64 {
    f64::from(x as f32)
}

impl Clock {
    pub fn new(rules_ptr: u64, roots: ClockRoots, state: u32, fields: [u32; 5]) -> Clock {
        Clock {
            rules_ptr,
            roots,
            state,
            fields,
            last_clock: None,
            last_paused: None,
        }
    }

    fn clear(&mut self, out: &mut Pass) {
        self.last_clock = None;
        self.last_paused = None;
        out.set(MATCH_INFO, "match_clock", None);
        out.set(MATCH_INFO, "game_paused", None);
    }

    fn pointers(&self, m: &dyn MemRead, rules: u64) -> Result<Pointers, TaskError> {
        let vtable = m.read_ptr(rules)?;
        Ok(Pointers {
            globals: m.read_ptr(self.roots.globals_ptr)?,
            vtable,
            offset: m.read_ptr(self.roots.offset_convar_ptr)?,
            pregame: m.read_ptr(self.roots.pregame_convar_ptr)?,
            enabled: m.read_ptr(self.roots.pregame_enabled_ptr)?,
        })
    }

    fn controls(&self, m: &dyn MemRead, rules: u64, p: &Pointers) -> Result<Controls, TaskError> {
        let [ticks, start_tick, paused, start, mode] = self.fields;
        Ok(Controls {
            paused_ticks: m.read_i32(at(rules, ticks.into())?)?,
            pause_start: m.read_i32(at(rules, start_tick.into())?)?,
            paused: m.read_u8(at(rules, paused.into())?)?,
            start: m.read_u32(at(rules, start.into())?)?,
            mode: m.read_i32(at(rules, mode.into())?)?,
            offset: m.read_u32(at(p.offset, CONVAR_VALUE)?)?,
            pregame: m.read_u32(at(p.pregame, CONVAR_VALUE)?)?,
            enabled: m.read_u8(at(p.enabled, CONVAR_VALUE)?)?,
        })
    }

    /// One consistent snapshot: every pointer and control read twice around the engine globals.
    fn snapshot(
        &self,
        m: &dyn MemRead,
        rules: u64,
    ) -> Result<(Controls, [u8; GLOBALS_BLOCK_LEN]), TaskError> {
        let p = self.pointers(m, rules)?;
        if m.read_ptr(at(p.vtable, VTABLE_ADJUST_SLOT)?)? != self.roots.adjust_time_fn {
            return Err(TaskError::Invalid(
                "unrecognized game-time implementation".into(),
            ));
        }
        let before = self.controls(m, rules, &p)?;
        let mut block = [0u8; GLOBALS_BLOCK_LEN];
        m.read(at(p.globals, GLOBALS_BLOCK_AT)?, &mut block)?;
        let after = self.controls(m, rules, &p)?;
        let again = self.pointers(m, rules)?;
        let same = (
            again.globals,
            again.vtable,
            again.offset,
            again.pregame,
            again.enabled,
        ) == (p.globals, p.vtable, p.offset, p.pregame, p.enabled);
        if !same || before != after {
            return Err(TaskError::Torn);
        }
        Ok((before, block))
    }
}

fn f32_at(b: &[u8], at: usize) -> f64 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&b[at..at + 4]);
    f64::from(f32::from_le_bytes(raw))
}

/// `"MM:SS"` for the game state in `c` and the engine globals `block`; `Torn` if the values are
/// not a coherent clock.
fn format_clock(c: &Controls, block: &[u8; GLOBALS_BLOCK_LEN]) -> Result<String, TaskError> {
    let current = f32_at(block, 0);
    let interval = f32_at(block, GLOBALS_INTERVAL_AT);
    let start = f64::from(f32::from_bits(c.start));
    let clock_offset = f64::from(f32::from_bits(c.offset));
    let pregame_offset = f64::from(f32::from_bits(c.pregame));
    let coherent = [current, interval, start, clock_offset, pregame_offset]
        .iter()
        .all(|v| v.is_finite())
        && interval > 0.0
        && interval <= 1.0
        && c.paused_ticks >= 0
        && c.pause_start >= 0
        && c.paused <= 1
        && c.enabled <= 1;
    if !coherent {
        return Err(TaskError::Torn);
    }
    let (ticks, pause_start) = (f64::from(c.paused_ticks), f64::from(c.pause_start));
    // The client freezes adjusted game time at the pause tick, once reached.
    let pause_time = f32r(pause_start * interval);
    let adjusted = if c.paused == 1 && c.pause_start > 0 && current >= pause_time {
        f32r((pause_start - ticks) * interval)
    } else {
        f32r(current - f32r(ticks * interval))
    };
    let mut seconds = f32r(f32r(adjusted - start) + clock_offset);
    if c.enabled == 1 && c.mode == 1 {
        seconds = f32r(seconds - pregame_offset);
    }
    if !(MIN_SECONDS..=MAX_SECONDS).contains(&seconds) {
        return Err(TaskError::Torn);
    }
    let whole = seconds.floor().max(0.0) as u64;
    Ok(format!("{:02}:{:02}", whole / 60, whole % 60))
}

impl Task for Clock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let Some(rules) = rules_object(m, self.rules_ptr)? else {
            self.clear(out);
            return Ok(());
        };
        if m.read_i32(at(rules, self.state.into())?)? != GAME_IN_PROGRESS {
            self.clear(out);
            return Ok(());
        }
        let (controls, block) = self.snapshot(m, rules)?;
        let clock = format_clock(&controls, &block)?;
        let paused = controls.paused == 1;

        if self.last_clock.as_deref() != Some(clock.as_str()) {
            out.event("clock", format!("{{\"match_clock\":\"{clock}\"}}"));
        }
        // Overwolf's `game_paused` event: data `true`/`false`, on each pause and unpause seen
        // between two passes of this match (the first pass has nothing to compare with).
        if self.last_paused.is_some_and(|was| was != paused) {
            out.event("game_paused", paused.to_string());
        }
        out.set(MATCH_INFO, "match_clock", Some(clock.clone()));
        out.set(MATCH_INFO, "game_paused", Some(paused.to_string()));
        self.last_clock = Some(clock);
        self.last_paused = Some(paused);
        Ok(())
    }
}
