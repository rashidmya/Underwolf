mod common;

use common::Synth;
use uw_spike_dlcollector::pattern::Pattern;
use uw_spike_dlcollector::roots::{
    ADJUST_TIME, CLOCK_RULE_FIELDS, CLOCK_SECONDS, CONVAR_GETTER, ClockRoots, GAME_TIME, RootError,
    resolve_clock_roots,
};

const GAME_TIME_AT: usize = 0x100;
const ADJUST_AT: usize = 0x200;
const GETTER_AT: usize = 0x300;
const CLOCK_AT: usize = 0x400;

struct Globals {
    globals: u64,
    offset: u64,
    pregame: u64,
    enabled: u64,
}

fn globals(s: &mut Synth) -> Globals {
    Globals {
        globals: s.alloc(&[0; 8]),
        // ConVar objects: the getter reads the data pointer at +8.
        offset: s.alloc(&[0; 16]),
        pregame: s.alloc(&[0; 16]),
        enabled: s.alloc(&[0; 8]),
    }
}

fn game_time_site(s: &mut Synth, at: usize, globals: u64) {
    s.place(at, GAME_TIME, &[(0x16, 0x1a, globals)]);
    // `je` over the paused branches to the unpaused `movss xmm2,[rax+0x30]` at +0x5f.
    s.poke(at + 0x1e, 0x40);
}

fn adjust_site(s: &mut Synth, at: usize, globals: u64) {
    s.place(at, ADJUST_TIME, &[(0x5f, 0x63, globals)]);
}

fn clock_site(s: &mut Synth, at: usize, g: &Globals, getter: u64, game_time: u64) {
    s.place(
        at,
        CLOCK_SECONDS,
        &[
            (8, 12, g.offset),
            (18, 22, getter),
            (25, 29, g.enabled),
            (53, 57, g.pregame),
            (58, 62, getter),
            (77, 81, game_time),
        ],
    );
}

/// All five patterns, optionally leaving one out.
fn image(skip: Option<&str>) -> (Synth, Globals) {
    let mut s = Synth::new();
    let g = globals(&mut s);
    if skip != Some("game time") {
        game_time_site(&mut s, GAME_TIME_AT, g.globals);
    }
    if skip != Some("adjust") {
        adjust_site(&mut s, ADJUST_AT, g.globals);
    }
    if skip != Some("getter") {
        s.place(GETTER_AT, CONVAR_GETTER, &[]);
    }
    if skip != Some("clock") {
        let (getter, game_time) = (s.code_va(GETTER_AT), s.code_va(GAME_TIME_AT));
        clock_site(&mut s, CLOCK_AT, &g, getter, game_time);
    }
    (s, g)
}

#[test]
fn clock_patterns_parse() {
    for p in [CLOCK_SECONDS, GAME_TIME, ADJUST_TIME, CONVAR_GETTER] {
        assert!(Pattern::parse(p).is_ok(), "{p}");
    }
}

#[test]
fn resolves_all_clock_roots() {
    let (s, g) = image(None);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Ok(ClockRoots {
            globals_ptr: g.globals,
            adjust_time_fn: s.code_va(ADJUST_AT),
            offset_convar_ptr: g.offset + 8,
            pregame_convar_ptr: g.pregame + 8,
            pregame_enabled_ptr: g.enabled,
        })
    );
}

#[test]
fn missing_pattern_is_not_found() {
    for (skip, what) in [
        ("game time", "game time globals"),
        ("adjust", "game-time adjust"),
        ("getter", "clock offset ConVar"),
        ("clock", "clock offset ConVar"),
    ] {
        let (s, _) = image(Some(skip));
        let (mem, module) = s.finish();
        assert_eq!(
            resolve_clock_roots(&mem, &module),
            Err(RootError::NotFound(what)),
            "without {skip}"
        );
    }
}

#[test]
fn adjust_must_read_the_same_globals() {
    let (mut s, _) = image(Some("adjust"));
    let other = s.alloc(&[0; 8]);
    adjust_site(&mut s, ADJUST_AT, other);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::NotFound("game-time adjust"))
    );
}

#[test]
fn clock_site_must_call_the_game_time_function() {
    let (mut s, g) = image(Some("clock"));
    let (getter, wrong) = (s.code_va(GETTER_AT), s.code_va(ADJUST_AT));
    clock_site(&mut s, CLOCK_AT, &g, getter, wrong);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::NotFound("clock offset ConVar"))
    );
}

#[test]
fn convar_pointers_must_be_outside_code() {
    let (mut s, mut g) = image(Some("clock"));
    g.enabled = s.code_va(0x10);
    let (getter, game_time) = (s.code_va(GETTER_AT), s.code_va(GAME_TIME_AT));
    clock_site(&mut s, CLOCK_AT, &g, getter, game_time);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::NotFound("pregame enabled ConVar"))
    );
}

#[test]
fn game_time_must_load_global_time_at_0x30() {
    let (mut s, _) = image(None);
    // `movss xmm2,[rax+0x30]` at +0x5f becomes `[rax+0x34]`.
    assert_eq!(s.code[GAME_TIME_AT + 0x63], 0x30);
    s.poke(GAME_TIME_AT + 0x63, 0x34);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::NotFound("game time globals"))
    );
}

#[test]
fn game_time_unpaused_branch_must_reach_the_0x30_load() {
    let (mut s, _) = image(None);
    s.poke(GAME_TIME_AT + 0x1e, 0x3a);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::NotFound("game time globals"))
    );
}

#[test]
fn two_game_time_functions_are_ambiguous() {
    let (mut s, _) = image(None);
    let other = s.alloc(&[0; 8]);
    game_time_site(&mut s, 0x600, other);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module),
        Err(RootError::Ambiguous("game time globals"))
    );
}

#[test]
fn identical_game_time_functions_are_one_root() {
    let (mut s, g) = image(None);
    game_time_site(&mut s, 0x600, g.globals);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_clock_roots(&mem, &module).unwrap().globals_ptr,
        g.globals
    );
}

#[test]
fn rule_fields_table_names_five_fields() {
    assert_eq!(CLOCK_RULE_FIELDS.len(), 5);
    for (_, class, field) in CLOCK_RULE_FIELDS {
        assert!(
            class.starts_with("C_") && field.starts_with("m_"),
            "{class}.{field}"
        );
    }
}
