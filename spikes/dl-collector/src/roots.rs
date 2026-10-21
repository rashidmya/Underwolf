use std::collections::BTreeSet;
use std::fmt;

use crate::mem::{MemRead, ReadError};
use crate::module::Module;
use crate::pattern::{Pattern, rip_target, scan};
use crate::timing::CallerFilter;

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

// Match clock roots (spec Task C3 clock). Found on Deadlock ClientVersion 6801 (client.dll
// SHA-256 a7d0d9e88d11f09a36294569662718394cfa36d99286774ecf8c9fd6ea4f589e). Each pattern is the
// owner's objdump of the previous build (d61348c5…, where underlock's clock was validated live)
// with rel32 displacements, jump offsets, stack offsets and game-rules field displacements
// masked; each matches exactly once on 6801. Kept fixed, because the clock reader depends on
// them: the ConVar value offset +0x58 (CLOCK_SECONDS, CONVAR_GETTER), the rules vtable slot
// +0x220 and global game time +0x30 on the unpaused path (GAME_TIME, plus a decoded check that
// its `je` lands on that load), and the tick interval +0x54 (ADJUST_TIME and GAME_TIME).

/// The match-clock seconds function (the `%d:%02d` formatter's callee): loads the clock-offset
/// ConVar, tests the pregame-enabled ConVar and the game mode, loads the pregame-offset ConVar,
/// then calls the game-time function. Decodes: ConVar object at +8 (data pointer at object +8),
/// getter call at +18, pregame-enabled data pointer at +25, pregame ConVar object at +53,
/// getter call at +58, game-time call at +77.
pub const CLOCK_SECONDS: &str = "0f 29 74 24 30 48 8d 0d ?? ?? ?? ?? 0f 29 7c 24 20 e8 ?? ?? ?? ?? 48 8b 05 ?? ?? ?? ?? 0f 28 f8 0f 57 f6 80 78 58 00 74 ?? 83 bb ?? ?? ?? ?? 01 75 ?? 48 8d 0d ?? ?? ?? ?? e8 ?? ?? ?? ?? 0f 28 f0 45 33 c0 48 8d 54 24 ?? 48 8b cb e8 ?? ?? ?? ?? 48 8d 53 ??";
/// The whole game-time function up to its unpaused tail call: fetches the rules' vtable slot
/// +0x220, loads the globals pointer (rel32 at +0x16), and when its flag argument is zero jumps
/// (`je` rel8 at +0x1e) to `movss xmm2,[rax+0x30]` at +0x5f, global time, and calls the slot.
/// The paused branches between (+0x23..+0x5f) read globals +0x40/+0x4c/+0x38/+0x54.
pub const GAME_TIME: &str = "40 53 48 83 ec 20 48 8b 01 48 8b da 4c 8b 90 20 02 00 00 48 8b 05 ?? ?? ?? ?? 45 85 c0 74 ?? 80 78 40 00 74 ?? f3 0f 10 50 4c 45 8b c8 41 ff d2 48 8b c3 48 83 c4 20 5b c3 f3 0f 10 50 38 45 8b c8 f3 0f 5c 15 ?? ?? ?? ?? f3 0f 59 50 54 f3 0f 58 50 4c 41 ff d2 48 8b c3 48 83 c4 20 5b c3 f3 0f 10 50 30 45 8b c8 41 ff d2";
/// GAME_TIME's `je` (rel8 at +0x1e, next instruction +0x1f) must land on its +0x30 load.
const GAME_TIME_JE: u64 = 0x1e;
const GAME_TIME_LOAD_0X30: u64 = 0x5f;
/// The game-time adjust function (C_CitadelGameRules vtable +0x220): subtracts total paused
/// ticks times the tick interval at globals +0x54; the globals load is the rel32 at +0x5f.
pub const ADJUST_TIME: &str = "48 89 5c 24 08 57 48 83 ec 30 0f 29 74 24 20 0f 28 f2 48 8b da 48 8b f9 45 85 c9 75 ?? 44 38 49 ?? 74 ?? 8b 49 ?? 85 c9 7f ?? 8b 0d ?? ?? ?? ?? ba 03 00 00 00 ff 15 ?? ?? ?? ?? 84 c0 74 ?? 8b 0d ?? ?? ?? ?? 4c 8d 05 ?? ?? ?? ?? ba 03 00 00 00 ff 15 ?? ?? ?? ?? 66 0f 6e 47 ?? 48 8b 05 ?? ?? ?? ?? 0f 5b c0 f3 0f 59 40 54 f3 0f 5c f0";
/// The float ConVar getter called by CLOCK_SECONDS: data pointer at object +8, value at +0x58.
pub const CONVAR_GETTER: &str = "48 8b 41 08 f3 0f 10 40 58 c3";
/// The ConVar getter reads the data pointer at this offset of the ConVar object.
const CONVAR_DATA_PTR: u64 = 8;

/// Game-rules fields of the clock arithmetic, by schema name (role, class, field). On 6801 they
/// sit at +0x30, +0x34, +0x38, +0x60 and +0xac, the offsets underlock read.
pub const CLOCK_RULE_FIELDS: [(&str, &str, &str); 5] = [
    ("total_paused_ticks", "C_GameRules", "m_nTotalPausedTicks"),
    ("pause_start_tick", "C_GameRules", "m_nPauseStartTick"),
    ("paused", "C_GameRules", "m_bGamePaused"),
    ("game_start_time", "C_CitadelGameRules", "m_flGameStartTime"),
    ("game_mode", "C_CitadelGameRules", "m_eGameMode"),
];

/// Addresses of the clock's roots: each `*_ptr` is a pointer global in the module (read at poll
/// time); `adjust_time_fn` is the function the rules vtable slot +0x220 must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockRoots {
    pub globals_ptr: u64,
    pub adjust_time_fn: u64,
    pub offset_convar_ptr: u64,
    pub pregame_convar_ptr: u64,
    pub pregame_enabled_ptr: u64,
}

/// Inside a non-executable section of the module.
fn in_data(module: &Module, addr: u64) -> bool {
    module.sections.iter().any(|s| {
        let start = module.base + s.rva as u64;
        !s.executable && addr >= start && addr - start < s.size as u64
    })
}

fn code_matches(m: &dyn MemRead, addr: u64, pattern: &str) -> bool {
    Pattern::parse(pattern).is_ok_and(|p| p.matches_mem(m, addr).unwrap_or(false))
}

/// The globals pointer loaded by the game-time function, and every game-time function site
/// that loads it.
fn game_time(m: &dyn MemRead, module: &Module) -> Result<(u64, BTreeSet<u64>), RootError> {
    let what = "game time globals";
    let mut sites = Vec::new();
    for site in find_sites(m, module, GAME_TIME, MAX_SITES, what)? {
        let je = m.read_u8(site + GAME_TIME_JE)? as i8;
        if site
            .wrapping_add(GAME_TIME_JE + 1)
            .wrapping_add(je as i64 as u64)
            != site + GAME_TIME_LOAD_0X30
        {
            continue;
        }
        let globals = rip_target(m, site, 0x16, 0x1a)?;
        if in_data(module, globals) {
            sites.push((site, globals));
        }
    }
    let globals = only(sites.iter().map(|&(_, g)| g).collect(), what)?;
    Ok((globals, sites.into_iter().map(|(s, _)| s).collect()))
}

fn adjust_time(m: &dyn MemRead, module: &Module, globals: u64) -> Result<u64, RootError> {
    let what = "game-time adjust";
    let mut found = BTreeSet::new();
    for site in find_sites(m, module, ADJUST_TIME, MAX_SITES, what)? {
        if rip_target(m, site, 0x5f, 0x63)? == globals {
            found.insert(site);
        }
    }
    only(found, what)
}

/// The three ConVar data-pointer globals, from CLOCK_SECONDS sites that call a game-time function and
/// the ConVar getter.
fn convars(
    m: &dyn MemRead,
    module: &Module,
    game_time_fns: &BTreeSet<u64>,
) -> Result<(u64, u64, u64), RootError> {
    let names = [
        "clock offset ConVar",
        "pregame offset ConVar",
        "pregame enabled ConVar",
    ];
    let mut found: [BTreeSet<u64>; 3] = Default::default();
    for site in find_sites(m, module, CLOCK_SECONDS, MAX_SITES, names[0])? {
        if !game_time_fns.contains(&rip_target(m, site, 77, 81)?) {
            continue;
        }
        let getters = [rip_target(m, site, 18, 22)?, rip_target(m, site, 58, 62)?];
        if !getters.iter().all(|&g| code_matches(m, g, CONVAR_GETTER)) {
            continue;
        }
        let ptrs = [
            rip_target(m, site, 8, 12)?.wrapping_add(CONVAR_DATA_PTR),
            rip_target(m, site, 53, 57)?.wrapping_add(CONVAR_DATA_PTR),
            rip_target(m, site, 25, 29)?,
        ];
        for (set, ptr) in found.iter_mut().zip(ptrs) {
            if in_data(module, ptr) {
                set.insert(ptr);
            }
        }
    }
    let [offset, pregame, enabled] = found;
    Ok((
        only(offset, names[0])?,
        only(pregame, names[1])?,
        only(enabled, names[2])?,
    ))
}

pub fn resolve_clock_roots(m: &dyn MemRead, client: &Module) -> Result<ClockRoots, RootError> {
    let (globals_ptr, game_time_fns) = game_time(m, client)?;
    let adjust_time_fn = adjust_time(m, client, globals_ptr)?;
    let (offset_convar_ptr, pregame_convar_ptr, pregame_enabled_ptr) =
        convars(m, client, &game_time_fns)?;
    Ok(ClockRoots {
        globals_ptr,
        adjust_time_fn,
        offset_convar_ptr,
        pregame_convar_ptr,
        pregame_enabled_ptr,
    })
}

// Main-loop QPC call site (spec §6.4 "Loading and timing"). Found with discovery mode
// (`discover=<s>`) in the Hideout: the chain on the game window's thread, inside a game module,
// firing at about the frame rate. `ret_offset` turns a pattern hit into the return address.
// Not yet discovered (Task C4 Step 5, owner session): until then the plugin reports
// `MAIN_LOOP_NOT_FOUND` and runs no passes.

/// A masked byte pattern in `module` whose hit plus `ret_offset` is a return address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerPattern {
    pub module: &'static str,
    pub pattern: &'static str,
    pub ret_offset: u64,
}

/// The game's QPC call on its main loop (the immediate caller).
pub const MAIN_LOOP_IMMEDIATE: Option<CallerPattern> = None;
/// The frame above it, when the game reaches QPC through a tier0 wrapper.
pub const MAIN_LOOP_OUTER: Option<CallerPattern> = None;

pub const MAIN_LOOP_NOT_FOUND: &str = "main-loop caller not found";
const MAIN_LOOP_AMBIGUOUS: &str = "main-loop caller is ambiguous";

fn caller_site(m: &dyn MemRead, modules: &[Module], p: &CallerPattern) -> Result<u64, String> {
    let module = modules
        .iter()
        .find(|md| md.name.eq_ignore_ascii_case(p.module))
        .ok_or(MAIN_LOOP_NOT_FOUND)?;
    match find_sites(m, module, p.pattern, 2, "main-loop caller") {
        Ok(sites) if sites.len() == 1 => Ok(sites[0].wrapping_add(p.ret_offset)),
        Ok(sites) if sites.len() > 1 => Err(MAIN_LOOP_AMBIGUOUS.into()),
        Err(RootError::Ambiguous(_)) => Err(MAIN_LOOP_AMBIGUOUS.into()),
        Ok(_) | Err(RootError::NotFound(_)) => Err(MAIN_LOOP_NOT_FOUND.into()),
        Err(e) => Err(format!("main-loop caller: {e}")),
    }
}

/// Resolves both patterns to exactly one hit each in their module; the outer one is required
/// when it is defined.
pub fn resolve_caller(
    m: &dyn MemRead,
    modules: &[Module],
    immediate: Option<&CallerPattern>,
    outer: Option<&CallerPattern>,
) -> Result<CallerFilter, String> {
    let immediate = caller_site(m, modules, immediate.ok_or(MAIN_LOOP_NOT_FOUND)?)?;
    let outer = outer.map(|p| caller_site(m, modules, p)).transpose()?;
    Ok(CallerFilter { immediate, outer })
}

pub fn resolve_main_loop(m: &dyn MemRead, modules: &[Module]) -> Result<CallerFilter, String> {
    resolve_caller(
        m,
        modules,
        MAIN_LOOP_IMMEDIATE.as_ref(),
        MAIN_LOOP_OUTER.as_ref(),
    )
}
