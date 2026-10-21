//! The layout table against the schema dump of build 6801 (`dumps/schema-6801.json`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uw_spike_dlcollector::layout::{RELATIONS, broken_tasks, holds};
use uw_spike_dlcollector::roots::{ClockRoots, Roots};
use uw_spike_dlcollector::schema::FieldLookup;
use uw_spike_dlcollector::tasks::{Built, TASK_FIELDS, build};

type Classes = BTreeMap<String, BTreeMap<String, u32>>;

/// How far `corrupt=` moves a field (`CORRUPT_DELTA` in `src/collector.rs`).
const MOVE: u32 = 8;

/// The dump as a field source. Records every lookup and answers `None` for `hidden`.
struct Dump {
    classes: Classes,
    hidden: Option<(&'static str, &'static str)>,
    seen: RefCell<BTreeSet<(String, String)>>,
}

impl Dump {
    fn new() -> Dump {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/dumps/schema-6801.json");
        let text = std::fs::read_to_string(path).unwrap();
        Dump {
            classes: serde_json::from_str(&text).unwrap(),
            hidden: None,
            seen: RefCell::default(),
        }
    }

    fn hiding(class: &'static str, field: &'static str) -> Dump {
        Dump {
            hidden: Some((class, field)),
            ..Dump::new()
        }
    }

    /// The dump with `class.field` moved by `MOVE`, as `corrupt=` does.
    fn moved(class: &str, field: &str) -> Dump {
        let mut d = Dump::new();
        let at = d.classes.get_mut(class).unwrap().get_mut(field).unwrap();
        *at += MOVE;
        d
    }
}

impl FieldLookup for Dump {
    fn get(&self, class: &str, field: &str) -> Option<u32> {
        self.seen.borrow_mut().insert((class.into(), field.into()));
        if self.hidden == Some((class, field)) {
            return None;
        }
        self.classes.get(class)?.get(field).copied()
    }
}

fn build_from(d: &Dump) -> Result<Built, String> {
    let roots = Roots {
        game_rules_ptr: 0x1000,
        teams: 0x1010,
        entity_list_ptr: 0x1030,
        entity_stride: 0x70,
        serial_offset: 0x10,
    };
    let clock = ClockRoots {
        globals_ptr: 0x1040,
        adjust_time_fn: 0x2000,
        offset_convar_ptr: 0x1048,
        pregame_convar_ptr: 0x1050,
        pregame_enabled_ptr: 0x1058,
    };
    build(&roots, Ok(clock), d)
}

fn names(b: &Built) -> BTreeSet<&'static str> {
    b.tasks.iter().map(|t| t.name()).collect()
}

fn invalid(task: &'static str, why: &str) -> (&'static str, String) {
    (task, why.into())
}

#[test]
fn every_relation_holds_in_the_6801_dump() {
    let d = Dump::new();
    for r in RELATIONS {
        assert!(holds(&d, r.class, r.field), "{}.{}", r.class, r.field);
    }
    assert!(broken_tasks(&d).is_empty(), "{:?}", broken_tasks(&d));
}

#[test]
fn each_anchor_is_the_nearest_lower_field_or_else_the_nearest_higher() {
    let d = Dump::new();
    for r in RELATIONS {
        let fields = &d.classes[r.class];
        let at = fields[r.field];
        let below = fields
            .iter()
            .filter(|&(_, &o)| o < at)
            .max_by_key(|&(_, &o)| o);
        let above = fields
            .iter()
            .filter(|&(_, &o)| o > at)
            .min_by_key(|&(_, &o)| o);
        let (anchor, _) = below.or(above).unwrap();
        assert_eq!(anchor, r.anchor, "{}.{}", r.class, r.field);
        assert_eq!(
            fields.values().filter(|&&o| o == fields[anchor]).count(),
            1,
            "{}.{}: the anchor shares its offset",
            r.class,
            r.field
        );
    }
}

/// Adding a field to a task without a relation (or without listing it) fails here.
#[test]
fn the_table_covers_exactly_the_fields_the_tasks_look_up() {
    let d = Dump::new();
    let built = build_from(&d).unwrap();
    assert!(built.unresolved.is_empty(), "{:?}", built.unresolved);
    let looked_up = d.seen.into_inner();
    let declared: BTreeSet<_> = TASK_FIELDS
        .iter()
        .map(|&(_, c, f)| (c.to_string(), f.to_string()))
        .collect();
    let table: BTreeSet<_> = RELATIONS
        .iter()
        .map(|r| (r.class.to_string(), r.field.to_string()))
        .collect();
    assert_eq!(looked_up, declared);
    assert_eq!(table, declared);
    assert_eq!(RELATIONS.len(), table.len(), "one relation per field");
}

#[test]
fn task_fields_name_every_task_that_needs_the_field() {
    let all = names(&build_from(&Dump::new()).unwrap());
    for &(task, class, field) in TASK_FIELDS {
        assert!(all.contains(task), "{task}");
        let users: BTreeSet<_> = TASK_FIELDS
            .iter()
            .filter(|t| (t.1, t.2) == (class, field))
            .map(|t| t.0)
            .collect();
        match build_from(&Dump::hiding(class, field)) {
            Ok(b) => {
                let gone: BTreeSet<_> = all.difference(&names(&b)).copied().collect();
                assert_eq!(gone, users, "{class}.{field}");
            }
            // A field of phase or match_id: nothing is built at all.
            Err(why) => assert!(
                why.contains(field) && (users.contains("phase") || users.contains("match_id")),
                "{class}.{field}: {why}"
            ),
        }
    }
}

#[test]
fn a_moved_field_breaks_only_the_tasks_that_read_it() {
    // `corrupt=PlayerDataGlobal_t.m_iPlayerKills`: the kills land on the assists.
    let d = Dump::moved("PlayerDataGlobal_t", "m_iPlayerKills");
    let data = &d.classes["PlayerDataGlobal_t"];
    assert_eq!(data["m_iPlayerKills"], data["m_iPlayerAssists"]);
    assert!(!holds(&d, "PlayerDataGlobal_t", "m_iPlayerKills"));
    assert_eq!(
        broken_tasks(&d),
        [invalid(
            "roster",
            "layout: PlayerDataGlobal_t.m_iPlayerKills"
        )]
    );

    let d = Dump::moved("C_CitadelGameRules", "m_eGameState");
    assert_eq!(
        broken_tasks(&d),
        [
            invalid("phase", "layout: C_CitadelGameRules.m_eGameState"),
            invalid("clock", "layout: C_CitadelGameRules.m_eGameState"),
            invalid("match_outcome", "layout: C_CitadelGameRules.m_eGameState"),
            invalid("tower_destroyed", "layout: C_CitadelGameRules.m_eGameState"),
        ]
    );

    // The team number is read by the roster, team_score and tower_destroyed.
    let d = Dump::moved("C_BaseEntity", "m_iTeamNum");
    assert_eq!(
        broken_tasks(&d),
        [
            invalid("roster", "layout: C_BaseEntity.m_iTeamNum"),
            invalid("team_score", "layout: C_BaseEntity.m_iTeamNum"),
            invalid("tower_destroyed", "layout: C_BaseEntity.m_iTeamNum"),
        ]
    );

    // Bosses alive moved onto m_nFlexSlotsUnlocked (it also anchors bosses max): only
    // tower_destroyed reads them.
    let d = Dump::moved("C_CitadelTeam", "m_nBossesAlive");
    assert_eq!(
        broken_tasks(&d),
        [invalid(
            "tower_destroyed",
            "layout: C_CitadelTeam.m_nBossesAlive"
        )]
    );
}

#[test]
fn a_moved_anchor_or_a_missing_one_breaks_the_relation() {
    // m_iPlayerAssists is read by the roster and is also the anchor of m_iDeaths.
    let d = Dump::moved("PlayerDataGlobal_t", "m_iPlayerAssists");
    assert!(!holds(&d, "PlayerDataGlobal_t", "m_iDeaths"));
    assert_eq!(
        broken_tasks(&d),
        [invalid("roster", "layout: PlayerDataGlobal_t.m_iDeaths")]
    );

    let mut d = Dump::new();
    d.classes
        .get_mut("C_CitadelGameRules")
        .unwrap()
        .remove("m_eMatchMode");
    // m_eMatchMode anchors the clock's m_eGameMode and is read by game_mode itself.
    assert_eq!(
        broken_tasks(&d),
        [
            invalid("clock", "layout: C_CitadelGameRules.m_eGameMode"),
            invalid("game_mode", "layout: C_CitadelGameRules.m_eMatchMode"),
        ]
    );
}

#[test]
fn a_field_without_a_relation_does_not_hold() {
    assert!(!holds(
        &Dump::new(),
        "C_CitadelGameRules",
        "m_bGGMarkAsNotScored"
    ));
    assert!(!holds(&Dump::new(), "C_Nope", "m_x"));
}
