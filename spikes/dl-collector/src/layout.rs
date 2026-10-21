//! Layout check (spec §6.4: "if layouts don't match, stop emitting rather than emit wrong
//! data"). Every schema field a task reads keeps a fixed distance to one neighbouring field of
//! the same class; a field that moved against its neighbour switches off the tasks that read it.
//! Most fields are plain integers, so no value check could tell a moved field from a real one.

use crate::schema::FieldLookup;
use crate::tasks::TASK_FIELDS;

/// `offset(class.field) - offset(class.anchor) == delta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relation {
    pub class: &'static str,
    pub field: &'static str,
    pub anchor: &'static str,
    pub delta: i64,
}

const fn rel(
    class: &'static str,
    field: &'static str,
    anchor: &'static str,
    delta: i64,
) -> Relation {
    Relation {
        class,
        field,
        anchor,
        delta,
    }
}

const RULES: &str = "C_CitadelGameRules";
const BASE_RULES: &str = "C_GameRules";
const CONTROLLER: &str = "CBasePlayerController";
const DATA: &str = "PlayerDataGlobal_t";

/// One relation per field in `TASK_FIELDS`. The anchor is the field's nearest lower-offset
/// neighbour in `dumps/schema-6801.json`, or the nearest higher one (negative delta) for the
/// lowest field of its class. Relations only: no absolute offsets.
pub const RELATIONS: [Relation; 23] = [
    rel(RULES, "m_eGameState", "m_flPlayOfTheGameStateEndTime", 4),
    rel(RULES, "m_unMatchID", "m_bGGMarkAsNotScored", 4),
    rel(RULES, "m_flGameStartTime", "m_fLevelStartTime", 4),
    rel(RULES, "m_eGameMode", "m_eMatchMode", 4),
    rel(RULES, "m_eMatchMode", "m_bAbandonTriggerAmber", 3),
    rel(RULES, "m_iWinningTeam", "m_hTrooperMinimap", 4),
    rel(BASE_RULES, "m_nTotalPausedTicks", "__m_pChainEntity", 40),
    rel(BASE_RULES, "m_nPauseStartTick", "m_nTotalPausedTicks", 4),
    rel(BASE_RULES, "m_bGamePaused", "m_nPauseStartTick", 4),
    rel("C_Team", "m_aPlayerControllers", "m_aPlayers", -24),
    rel("C_Team", "m_iScore", "m_aPlayers", 24),
    rel(
        "C_CitadelTeam",
        "m_nBossesAlive",
        "m_bHasValidBaseLocation",
        4,
    ),
    rel("C_CitadelTeam", "m_nBossesMax", "m_nBossesAlive", 4),
    rel("C_BaseEntity", "m_iTeamNum", "m_bClientSideRagdoll", 1),
    rel(CONTROLLER, "m_iszPlayerName", "m_iMostConnected", 4),
    rel(CONTROLLER, "m_steamID", "m_iszPlayerName", 140),
    rel(CONTROLLER, "m_bIsLocalPlayerController", "m_steamID", 8),
    rel(
        "CCitadelPlayerController",
        "m_PlayerDataGlobal",
        "m_hHeroPawn",
        72,
    ),
    rel(DATA, "m_nHeroID", "m_flRespawnTime", 4),
    rel(DATA, "m_iPlayerKills", "m_iHealth", 4),
    rel(DATA, "m_iDeaths", "m_iPlayerAssists", 4),
    rel(DATA, "m_iPlayerAssists", "m_iNPCKills", 4),
    rel(DATA, "m_vecUpgrades", "m_iObjectiveDamage", 4),
];

/// Whether `class.field` keeps its relation in `fields`. False when the table has no relation
/// for it, when the field or its anchor is missing, or when their distance is not the delta.
pub fn holds(fields: &dyn FieldLookup, class: &str, field: &str) -> bool {
    let Some(r) = RELATIONS
        .iter()
        .find(|r| r.class == class && r.field == field)
    else {
        return false;
    };
    match (fields.get(class, field), fields.get(class, r.anchor)) {
        (Some(at), Some(anchor)) => i64::from(at) - i64::from(anchor) == r.delta,
        _ => false,
    }
}

/// Every task with a field in `TASK_FIELDS` whose relation fails in `fields`, once, with the
/// reason `layout: <Class>.<field>` for its first such field (for `TaskError::Invalid`, see
/// `Runner::disable`).
pub fn broken_tasks(fields: &dyn FieldLookup) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for &(task, class, field) in TASK_FIELDS {
        if out.iter().any(|(t, _)| *t == task) || holds(fields, class, field) {
            continue;
        }
        out.push((task, format!("layout: {class}.{field}")));
    }
    out
}
