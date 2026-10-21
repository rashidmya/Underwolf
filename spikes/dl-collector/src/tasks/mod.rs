//! The collection tasks and the resolution of what each one needs.

mod clock;
mod match_id;
mod phase;
mod roster;

use crate::mem::{MemRead, PTR_MAX, PTR_MIN, ReadError};
use crate::roots::{CLOCK_RULE_FIELDS, ClockRoots, RootError, Roots};
use crate::runner::{Task, TaskError};
use crate::schema::SchemaFields;

pub use clock::Clock;
pub use match_id::MatchId;
pub use phase::Phase;
pub use roster::{Roster, RosterFields};

/// Field offsets of 0 or beyond this are not believed (a broken schema read, not a field).
const FIELD_OFFSET_MAX: u32 = 1 << 20;

const RULES: &str = "C_CitadelGameRules";

pub struct Built {
    pub tasks: Vec<Box<dyn Task>>,
    /// Tasks that were not created, with `"unresolved: <what>"`.
    pub unresolved: Vec<(&'static str, String)>,
}

fn offset(f: &SchemaFields, class: &str, field: &str) -> Option<u32> {
    f.get(class, field)
        .filter(|&o| o != 0 && o <= FIELD_OFFSET_MAX)
}

fn required(f: &SchemaFields, class: &str, field: &str) -> Result<u32, String> {
    offset(f, class, field).ok_or_else(|| format!("required field {class}.{field} not found"))
}

/// Creates every task whose roots and fields resolved. `Err` carries the reason a required
/// field (`m_eGameState`, `m_unMatchID`) is missing; any other gap only leaves its task out.
pub fn build(
    roots: &Roots,
    clock: Result<ClockRoots, RootError>,
    fields: &SchemaFields,
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
