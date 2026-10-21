use std::collections::BTreeSet;
use std::fmt;

use crate::mem::{MemRead, ReadError};
use crate::module::Module;
use crate::pattern::{Pattern, rip_target, scan};

// Byte patterns and checks come from the owner's reader
// ~/Dev/underlock/src/games/native/source2.js, verified there on Deadlock ClientVersion 6801
// (client.dll SHA-256 a7d0d9e88d11f09a36294569662718394cfa36d99286774ecf8c9fd6ea4f589e).
pub const RULES: &str = "48 89 1d ?? ?? ?? ?? ff 15 ?? ?? ?? ?? 84 c0 74 ?? 8b 0d ?? ?? ?? ?? 4c 8d 0d ?? ?? ?? ?? 4c 8d 05 ?? ?? ?? ??";
pub const CLASSES: &str = "48 8d 15 ?? ?? ?? ?? 48 89 ?? ?? ?? 4c 8d 0d ?? ?? ?? ?? 48 89 ?? ?? ?? 4c 8d 05 ?? ?? ?? ?? 48 8b ?? c7 44 ?? ?? 76 00 00 00 48 8b ?? ff 90 ?? 01 00 00";
pub const TEAMS: &str =
    "85 c9 78 ?? 3b 0d ?? ?? ?? ?? 7d ?? 48 8b 05 ?? ?? ?? ?? 48 63 c9 48 8b 04 c8 c3 33 c0 c3";
// Narrow, decoded register variant: stride and serial displacement come from immediates.
pub const ENTITIES: &str = "4c 8b 05 ?? ?? ?? ?? 4d 85 c0 74 ?? 83 f9 fe 74 ?? 8b c1 25 ff 7f 00 00 8b d0 48 c1 e8 09 4d 8b 0c c0 4d 85 c9 74 ?? 81 e2 ff 01 00 00 48 6b c2 ?? 49 03 c1 74 ?? 39 48 ??";

pub const RULES_MARKER: &str = "%s:  CGameRules::CGameRules constructed\n";

/// Signature scans stop at this many candidate sites (RULES/CLASSES/TEAMS) ...
pub const MAX_SITES: usize = 128;
/// ... or this many for the looser ENTITIES pattern.
pub const MAX_ENTITY_SITES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootError {
    NotFound(&'static str),
    Ambiguous(&'static str),
    Read(ReadError),
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RootError::NotFound(what) => write!(f, "{what} not found"),
            RootError::Ambiguous(what) => write!(f, "{what} is ambiguous"),
            RootError::Read(e) => write!(f, "read failed: {e}"),
        }
    }
}

impl std::error::Error for RootError {}

impl From<ReadError> for RootError {
    fn from(e: ReadError) -> RootError {
        RootError::Read(e)
    }
}

/// Addresses (not values) of the game's roots: each `*_ptr` and `teams` is the address of a
/// global in the module, to be read at poll time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub game_rules_ptr: u64,
    pub teams: u64,
    pub entity_list_ptr: u64,
    pub entity_stride: u32,
    pub serial_offset: u32,
}

/// Scans for `pattern`; a pattern that cannot be parsed or a scan past `limit` is reported
/// against `what`.
pub(crate) fn find_sites(
    m: &dyn MemRead,
    module: &Module,
    pattern: &str,
    limit: usize,
    what: &'static str,
) -> Result<Vec<u64>, RootError> {
    let p = Pattern::parse(pattern).map_err(|_| RootError::NotFound(what))?;
    scan(m, module, &p, limit).map_err(|e| match e {
        ReadError::LimitExceeded => RootError::Ambiguous(what),
        e => RootError::Read(e),
    })
}

fn only<T: Ord>(found: BTreeSet<T>, what: &'static str) -> Result<T, RootError> {
    let mut it = found.into_iter();
    match (it.next(), it.next()) {
        (None, _) => Err(RootError::NotFound(what)),
        (Some(v), None) => Ok(v),
        (Some(_), Some(_)) => Err(RootError::Ambiguous(what)),
    }
}

fn game_rules(m: &dyn MemRead, module: &Module) -> Result<u64, RootError> {
    let mut found = BTreeSet::new();
    for site in find_sites(m, module, RULES, MAX_SITES, "game rules")? {
        // A signature candidate is not a validated reference until the marker string matches.
        let Ok(string) = rip_target(m, site, 0x21, 0x25) else {
            continue;
        };
        if !module.contains(string) || m.read_cstr(string, 128).as_deref() != Ok(RULES_MARKER) {
            continue;
        }
        let target = rip_target(m, site, 3, 7)?;
        if module.contains(target) {
            found.insert(target);
        }
    }
    only(found, "game rules")
}

fn teams(m: &dyn MemRead, module: &Module) -> Result<u64, RootError> {
    let mut found = BTreeSet::new();
    for site in find_sites(m, module, TEAMS, MAX_SITES, "teams")? {
        let root = rip_target(m, site, 6, 10)?;
        if rip_target(m, site, 15, 19)? == root.wrapping_add(8) && module.contains(root) {
            found.insert(root);
        }
    }
    only(found, "teams")
}

fn entities(m: &dyn MemRead, module: &Module) -> Result<(u64, u32, u32), RootError> {
    let mut found = BTreeSet::new();
    for site in find_sites(m, module, ENTITIES, MAX_ENTITY_SITES, "entity list")? {
        let stride = m.read_u8(site + 48)? as u32;
        let serial = m.read_u8(site + 56)? as u32;
        if !(24..128).contains(&stride) || serial < 8 || serial + 4 > stride {
            continue;
        }
        let list = rip_target(m, site, 3, 7)?;
        if module.contains(list) {
            found.insert((list, stride, serial));
        }
    }
    only(found, "entity list")
}

pub fn resolve_roots(m: &dyn MemRead, client: &Module) -> Result<Roots, RootError> {
    let game_rules_ptr = game_rules(m, client)?;
    let teams = teams(m, client)?;
    let (entity_list_ptr, entity_stride, serial_offset) = entities(m, client)?;
    Ok(Roots {
        game_rules_ptr,
        teams,
        entity_list_ptr,
        entity_stride,
        serial_offset,
    })
}
