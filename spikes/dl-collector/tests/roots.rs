mod common;

use common::{BASE, Synth};
use uw_spike_dlcollector::pattern::Pattern;
use uw_spike_dlcollector::roots::{CLASSES, ENTITIES, RULES, RootError, TEAMS, resolve_roots};

const MARKER: &str = "%s:  CGameRules::CGameRules constructed\n";

fn rules_site(s: &mut Synth, at: usize, global: u64, marker: &str) {
    let m = s.cstr(marker);
    s.place(at, RULES, &[(3, 7, global), (0x21, 0x25, m)]);
}

fn teams_site(s: &mut Synth, at: usize, root: u64, second: u64) {
    s.place(at, TEAMS, &[(6, 10, root), (15, 19, second)]);
}

fn entities_site(s: &mut Synth, at: usize, list: u64, stride: u8, serial: u8) {
    s.place(at, ENTITIES, &[(3, 7, list)]);
    s.poke(at + 48, stride);
    s.poke(at + 56, serial);
}

fn globals(s: &mut Synth) -> (u64, u64, u64) {
    (s.alloc(&[0; 8]), s.alloc(&[0; 32]), s.alloc(&[0; 8]))
}

#[test]
fn patterns_parse() {
    for p in [RULES, CLASSES, TEAMS, ENTITIES] {
        assert!(Pattern::parse(p).is_ok(), "{p}");
    }
}

#[test]
fn rules_requires_marker_string() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, "%s:  something else\n");
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_roots(&mem, &module),
        Err(RootError::NotFound("game rules"))
    );
}

#[test]
fn resolves_all_roots() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    let r = resolve_roots(&mem, &module).unwrap();
    assert_eq!(r.game_rules_ptr, rules);
    assert_eq!(r.teams, teams);
    assert_eq!(r.entity_list_ptr, ents);
    assert_eq!((r.entity_stride, r.serial_offset), (0x70, 0x10));
    assert!(rules > BASE);
}

#[test]
fn two_distinct_rules_references_are_ambiguous() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    let other = s.alloc(&[0; 8]);
    rules_site(&mut s, 0x100, rules, MARKER);
    rules_site(&mut s, 0x180, other, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_roots(&mem, &module),
        Err(RootError::Ambiguous("game rules"))
    );
}

#[test]
fn identical_rules_references_are_one_root() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, MARKER);
    rules_site(&mut s, 0x180, rules, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(resolve_roots(&mem, &module).unwrap().game_rules_ptr, rules);
}

#[test]
fn teams_requires_second_reference_at_root_plus_8() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 16);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_roots(&mem, &module),
        Err(RootError::NotFound("teams"))
    );
}

#[test]
fn entities_reject_implausible_stride_and_serial() {
    for (stride, serial) in [(16u8, 8u8), (128, 8), (0x70, 4), (0x70, 0x6d)] {
        let mut s = Synth::new();
        let (rules, teams, ents) = globals(&mut s);
        rules_site(&mut s, 0x100, rules, MARKER);
        teams_site(&mut s, 0x200, teams, teams + 8);
        entities_site(&mut s, 0x300, ents, stride, serial);
        let (mem, module) = s.finish();
        assert_eq!(
            resolve_roots(&mem, &module),
            Err(RootError::NotFound("entity list")),
            "stride {stride} serial {serial}"
        );
    }
}

#[test]
fn entities_hits_must_agree() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    entities_site(&mut s, 0x400, ents, 0x78, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(
        resolve_roots(&mem, &module),
        Err(RootError::Ambiguous("entity list"))
    );
}

#[test]
fn entities_hits_that_agree_are_accepted() {
    let mut s = Synth::new();
    let (rules, teams, ents) = globals(&mut s);
    rules_site(&mut s, 0x100, rules, MARKER);
    teams_site(&mut s, 0x200, teams, teams + 8);
    entities_site(&mut s, 0x300, ents, 0x70, 0x10);
    entities_site(&mut s, 0x400, ents, 0x70, 0x10);
    let (mem, module) = s.finish();
    assert_eq!(resolve_roots(&mem, &module).unwrap().entity_stride, 0x70);
}
