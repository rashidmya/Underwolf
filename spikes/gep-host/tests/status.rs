use uw_spike_gep_host::provider::Emission;
use uw_spike_gep_host::status::{EVENTS, KEYS, StatusTable};

fn info(category: &str, key: &str, value: Option<&str>) -> Emission {
    Emission::Info { feature: category.into(), category: category.into(), key: key.into(), value: value.map(Into::into) }
}

fn event(name: &str) -> Emission {
    Emission::Event { name: name.into(), data: "{}".into() }
}

#[test]
fn status_marks_disabled_and_unavailable() {
    let mut t = StatusTable::default();
    t.observe(&info("gep_internal", "disabled_tasks", Some(r#"[{"task":"clock","reason":"slow"}]"#)));
    t.observe(&info("game_info", "phase", Some("GameInProgress")));
    assert_eq!(t.row("match_clock").as_deref(), Some("disabled: slow"));
    assert_eq!(t.row("game_paused").as_deref(), Some("disabled: slow"));
    assert_eq!(t.row("team_score").as_deref(), Some("unavailable"));
    assert_eq!(t.row("phase").as_deref(), Some("supported"));
}

#[test]
fn status_has_one_row_per_spec_key_and_event() {
    let t = StatusTable::default();
    let names: Vec<&str> = t.rows().iter().map(|(n, _)| *n).collect();
    assert_eq!(names.len(), KEYS.len() + EVENTS.len());
    for name in [
        "version_info", "phase", "game_mode", "match_history", "steam_id", "team_score", "match_id", "roster",
        "items", "incoming_damage", "match_outcome", "match_start", "match_end", "kill", "death", "assist",
        "match_clock", "game_paused", "tower_destroyed",
    ] {
        assert_eq!(t.row(name).as_deref(), Some("unavailable"), "{name}");
    }
    assert_eq!(t.row("selftest"), None);
}

#[test]
fn seen_values_are_supported_and_cleared_values_do_not_count() {
    let mut t = StatusTable::default();
    t.observe(&info("match_info", "roster_7", Some("{}")));
    t.observe(&info("match_info", "items_0", None));
    t.observe(&info("game_info", "steam_id", Some("76561198000000001")));
    t.observe(&event("kill"));
    t.observe(&event("match_clock"));
    assert_eq!(t.row("roster").as_deref(), Some("supported"));
    assert_eq!(t.row("items").as_deref(), Some("unavailable"));
    assert_eq!(t.row("steam_id").as_deref(), Some("supported"));
    assert_eq!(t.row("kill").as_deref(), Some("supported"));
    assert_eq!(t.row("match_clock").as_deref(), Some("supported"));
    assert_eq!(t.row("death").as_deref(), Some("unavailable"));
}

#[test]
fn disabled_roster_covers_its_keys_and_events() {
    let mut t = StatusTable::default();
    t.observe(&info(
        "gep_internal",
        "disabled_tasks",
        Some(r#"[{"task":"roster","reason":"invalid: duplicate player identity"}]"#),
    ));
    for name in ["roster", "items", "steam_id", "kill", "death", "assist"] {
        assert_eq!(t.row(name).as_deref(), Some("disabled: invalid: duplicate player identity"), "{name}");
    }
    assert_eq!(t.row("phase").as_deref(), Some("unavailable"));
}

#[test]
fn plugin_error_marks_every_row() {
    let mut t = StatusTable::default();
    t.observe(&info("game_info", "phase", Some("GameInProgress")));
    t.observe(&info("gep_internal", "plugin_status", Some("error")));
    t.observe(&info("gep_internal", "plugin_error", Some("main-loop caller not found")));
    assert_eq!(t.row("phase").as_deref(), Some("error: main-loop caller not found"));
    assert_eq!(t.row("kill").as_deref(), Some("error: main-loop caller not found"));
    // Back to scanning (a new plugin start): the error no longer applies.
    t.observe(&info("gep_internal", "plugin_status", Some("scanning")));
    assert_eq!(t.row("phase").as_deref(), Some("supported"));
}

#[test]
fn every_plugin_status_is_understood() {
    for s in [
        "initializing", "initialized", "scanning", "stalled", "selftest", "discovering", "discovered", "error",
        "shutdown",
    ] {
        let mut t = StatusTable::default();
        t.observe(&info("gep_internal", "plugin_status", Some(s)));
        let text = t.plugin_status();
        assert!(text.starts_with(s), "{s}: {text}");
        assert!(!text.contains("unknown"), "{s}: {text}");
    }
    let mut t = StatusTable::default();
    assert!(t.plugin_status().contains("no status"));
    t.observe(&info("gep_internal", "plugin_status", Some("weird")));
    assert!(t.plugin_status().contains("unknown"));
}

#[test]
fn render_lists_every_row() {
    let mut t = StatusTable::default();
    t.observe(&info("gep_internal", "plugin_status", Some("selftest")));
    let text = t.render();
    assert!(text.contains("selftest"));
    for (name, value) in t.rows() {
        assert!(text.lines().any(|l| l.contains(name) && l.contains(&value)), "{name}");
    }
}

#[test]
fn info_values_never_mark_event_rows_supported() {
    let mut t = StatusTable::default();
    t.observe(&info("match_info", "game_paused", Some("false")));
    t.observe(&info("match_info", "match_clock", Some("01:02")));
    assert_eq!(t.row("game_paused").as_deref(), Some("unavailable"));
    assert_eq!(t.row("match_clock").as_deref(), Some("unavailable"));
    // The events themselves still count, and a key only counts under its own category.
    t.observe(&event("game_paused"));
    assert_eq!(t.row("game_paused").as_deref(), Some("supported"));
    t.observe(&info("game_info", "match_id", Some("1")));
    assert_eq!(t.row("match_id").as_deref(), Some("unavailable"));
}
