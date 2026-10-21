mod common;

use common::Synth;
use uw_spike_dlcollector::roots::RootError;
use uw_spike_dlcollector::schema::SchemaFields;

fn base_synth() -> Synth {
    let mut s = Synth::new();
    let rules = s.class(
        "C_CitadelGameRules",
        &[
            ("m_eGameState", 0x74),
            ("m_unMatchID", 0xa060),
            ("m_flMatchClockAtLastUpdate", 0x9fdc),
        ],
    );
    let team = s.class("C_Team", &[("m_aPlayerControllers", 0x5f0)]);
    let table = s.class_table(&[rules, team]);
    s.classes_site(0x100, table);
    s
}

#[test]
fn schema_resolves_class_qualified_field() {
    let s = base_synth();
    let (mem, module) = s.finish();
    let f = SchemaFields::load(&mem, &module).unwrap();
    assert_eq!(f.get("C_CitadelGameRules", "m_eGameState"), Some(0x74));
    assert_eq!(f.get("C_CitadelGameRules", "m_unMatchID"), Some(0xa060));
    assert_eq!(f.get("C_Team", "m_aPlayerControllers"), Some(0x5f0));
    assert_eq!(f.get("C_CitadelGameRules", "m_nope"), None);
    assert_eq!(f.get("C_Nope", "m_eGameState"), None);
    // Field names are qualified by class: the same name in another class is not a match.
    assert_eq!(f.get("C_Team", "m_eGameState"), None);
    assert!(f.ambiguous().is_empty());
    assert_eq!(f.layout_name(), "disk-class-v2");
}

#[test]
fn duplicate_class_is_ambiguous() {
    let mut s = Synth::new();
    let rules = s.class("C_CitadelGameRules", &[("m_eGameState", 0x74)]);
    let team_a = s.class("C_Team", &[("m_aPlayerControllers", 0x5f0)]);
    let team_b = s.class("C_Team", &[("m_aPlayerControllers", 0x600)]);
    let table = s.class_table(&[rules, team_a, team_b]);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    let f = SchemaFields::load(&mem, &module).unwrap();
    assert_eq!(f.get("C_Team", "m_aPlayerControllers"), None);
    assert_eq!(f.get("C_CitadelGameRules", "m_eGameState"), Some(0x74));
    assert_eq!(f.ambiguous().len(), 1);
    assert!(f.ambiguous()[0].contains("C_Team"), "{:?}", f.ambiguous());
}

#[test]
fn duplicate_field_is_ambiguous_for_that_field_only() {
    let mut s = Synth::new();
    let rules = s.class("C_CitadelGameRules", &[("m_eGameState", 0x74)]);
    let team = s.class("C_Team", &[("m_x", 1), ("m_x", 2), ("m_y", 3)]);
    let table = s.class_table(&[rules, team]);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    let f = SchemaFields::load(&mem, &module).unwrap();
    assert_eq!(f.get("C_Team", "m_x"), None);
    assert_eq!(f.get("C_Team", "m_y"), Some(3));
    assert!(
        f.ambiguous()
            .iter()
            .any(|r| r.contains("C_Team") && r.contains("m_x"))
    );
}

#[test]
fn missing_class_table_is_not_found() {
    let s = Synth::new();
    let (mem, module) = s.finish();
    assert_eq!(
        SchemaFields::load(&mem, &module).err(),
        Some(RootError::NotFound("schema class table"))
    );
}

#[test]
fn unterminated_class_table_is_rejected() {
    // A table whose first entry points to itself never reaches the NUL terminator within the
    // 20000-entry bound, so it is not accepted as a class table.
    let mut s = Synth::new();
    let rules = s.class("C_CitadelGameRules", &[("m_eGameState", 0x74)]);
    let mut b = Vec::new();
    for _ in 0..20_001 {
        b.extend_from_slice(&rules.to_le_bytes());
    }
    let table = s.alloc(&b);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    assert_eq!(
        SchemaFields::load(&mem, &module).err(),
        Some(RootError::NotFound("schema class table"))
    );
}

#[test]
fn missing_probe_class_means_unknown_layout() {
    let mut s = Synth::new();
    let team = s.class("C_Team", &[("m_aPlayerControllers", 0x5f0)]);
    let table = s.class_table(&[team]);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    assert_eq!(
        SchemaFields::load(&mem, &module).err(),
        Some(RootError::NotFound("schema layout"))
    );
}

#[test]
fn field_count_above_bound_skips_class() {
    let mut s = Synth::new();
    let rules = s.class("C_CitadelGameRules", &[("m_eGameState", 0x74)]);
    let bad = s.class("C_Huge", &[("m_a", 1)]);
    // Patch the count of C_Huge to 5000 (> 4096).
    let off = (bad - common::BASE - common::DATA_RVA as u64) as usize + 0x24;
    s.data[off..off + 2].copy_from_slice(&5000u16.to_le_bytes());
    let table = s.class_table(&[rules, bad]);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    let f = SchemaFields::load(&mem, &module).unwrap();
    assert_eq!(f.get("C_Huge", "m_a"), None);
    assert_eq!(f.get("C_CitadelGameRules", "m_eGameState"), Some(0x74));
    assert_eq!(f.unreadable(), ["C_Huge".to_string()]);
}

#[test]
fn add_offset_shifts_one_resolved_field() {
    let (mem, module) = base_synth().finish();
    let mut f = SchemaFields::load(&mem, &module).unwrap();
    assert!(f.add_offset("C_CitadelGameRules", "m_eGameState", 8));
    assert_eq!(f.get("C_CitadelGameRules", "m_eGameState"), Some(0x7c));
    assert_eq!(f.get("C_CitadelGameRules", "m_unMatchID"), Some(0xa060));
    assert!(!f.add_offset("C_CitadelGameRules", "m_nope", 8));
    assert!(!f.add_offset("C_Nope", "m_eGameState", 8));
    assert!(!f.add_offset("C_CitadelGameRules", "m_unMatchID", u32::MAX));
    assert_eq!(f.get("C_CitadelGameRules", "m_unMatchID"), Some(0xa060));
}
