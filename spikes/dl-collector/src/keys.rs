//! The fixed info schema the collector publishes (spec §6.4); max lengths are in bytes.

use uw_spike_gep_channel::Schema;

pub const GEP_INTERNAL: &str = "gep_internal";
pub const GAME_INFO: &str = "game_info";
pub const MATCH_INFO: &str = "match_info";

/// Roster and item slots; slot numbers are stable per player identity.
pub const SLOTS: usize = 12;
pub const ROSTER_MAX: u32 = 1024;
/// 64 upgrades of up to 18 bytes each, plus a 128-byte name (escaped), need more than 1024.
pub const ITEMS_MAX: u32 = 2048;
pub const DISABLED_TASKS_MAX: u32 = 1024;
pub const PLUGIN_STATUS_MAX: u32 = 32;
pub const PLUGIN_ERROR_MAX: u32 = 256;

pub fn roster_key(slot: usize) -> String {
    format!("roster_{slot}")
}

pub fn items_key(slot: usize) -> String {
    format!("items_{slot}")
}

pub fn deadlock_schema() -> Schema {
    let key = |k: &str, max: u32| (k.to_string(), max);
    let mut match_info = vec![
        key("match_id", 32),
        key("match_clock", 16),
        key("game_paused", 8),
        key("team_score", 64),
        key("match_outcome", 64),
    ];
    match_info.extend((0..SLOTS).map(|i| (roster_key(i), ROSTER_MAX)));
    match_info.extend((0..SLOTS).map(|i| (items_key(i), ITEMS_MAX)));
    Schema {
        categories: vec![
            (
                GEP_INTERNAL.into(),
                vec![
                    key("version_info", 128),
                    key("plugin_status", PLUGIN_STATUS_MAX),
                    key("plugin_error", PLUGIN_ERROR_MAX),
                    key("disabled_tasks", DISABLED_TASKS_MAX),
                    key("pass_stats", 128),
                ],
            ),
            (
                GAME_INFO.into(),
                vec![key("phase", 64), key("game_mode", 256)],
            ),
            (MATCH_INFO.into(), match_info),
        ],
    }
}
