//! The feature-status table: one row per spec §6.4 key and event, from what this run emitted and
//! what the collector reported about itself (`plugin_status`, `plugin_error`, `disabled_tasks`).
//! Precedence: `error: <reason>` (the collector is in its error state) > `disabled: <reason>`
//! (the task feeding the row was switched off) > `supported` (seen at least once) > `unavailable`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::provider::Emission;

/// Spec §6.4 info keys, `(category, key)`.
pub const KEYS: [(&str, &str); 11] = [
    ("gep_internal", "version_info"),
    ("game_info", "phase"),
    ("game_info", "game_mode"),
    ("game_info", "match_history"),
    ("game_info", "steam_id"),
    ("match_info", "team_score"),
    ("match_info", "match_id"),
    ("match_info", "roster"),
    ("match_info", "items"),
    ("match_info", "incoming_damage"),
    ("match_info", "match_outcome"),
];

/// Spec §6.4 events.
pub const EVENTS: [&str; 8] =
    ["match_start", "match_end", "kill", "death", "assist", "match_clock", "game_paused", "tower_destroyed"];

/// Rows fed by each collector task (a task named like a row feeds that row).
const TASK_ROWS: [(&str, &[&str]); 4] = [
    ("phase", &["phase", "match_start", "match_end"]),
    ("match_id", &["match_id", "match_start", "match_end"]),
    ("roster", &["roster", "items", "steam_id", "kill", "death", "assist"]),
    ("clock", &["match_clock", "game_paused"]),
];

#[derive(Debug, Default)]
pub struct StatusTable {
    seen: BTreeSet<String>,
    /// Row → reason, from the latest `disabled_tasks`.
    disabled: BTreeMap<String, String>,
    plugin_status: Option<String>,
    plugin_error: Option<String>,
}

fn is_row(name: &str) -> bool {
    KEYS.iter().any(|&(_, k)| k == name) || EVENTS.contains(&name)
}

/// `roster_3` → `roster`, `items_0` → `items`; other keys are their own row.
fn row_for_key(key: &str) -> &str {
    match key.rsplit_once('_') {
        Some((base @ ("roster" | "items"), n)) if n.parse::<u32>().is_ok() => base,
        _ => key,
    }
}

fn rows_for_task(task: &str) -> Vec<&str> {
    match TASK_ROWS.iter().find(|(t, _)| *t == task) {
        Some((_, rows)) => rows.to_vec(),
        None if is_row(task) => vec![task],
        None => Vec::new(),
    }
}

impl StatusTable {
    pub fn observe(&mut self, e: &Emission) {
        match e {
            Emission::Info { category, key, value, .. } => {
                if category == "gep_internal" {
                    match key.as_str() {
                        "plugin_status" => self.plugin_status = value.clone(),
                        "plugin_error" => self.plugin_error = value.clone(),
                        "disabled_tasks" => self.set_disabled(value.as_deref()),
                        _ => {}
                    }
                }
                let row = row_for_key(key);
                if value.is_some() && is_row(row) {
                    self.seen.insert(row.into());
                }
            }
            Emission::Event { name, .. } => {
                if is_row(name) {
                    self.seen.insert(name.clone());
                }
            }
        }
    }

    fn set_disabled(&mut self, json: Option<&str>) {
        self.disabled.clear();
        let Some(json) = json else { return };
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(json) else {
            eprintln!("[uw-gep] disabled_tasks is not a JSON array: {json}");
            return;
        };
        for entry in entries {
            let (Some(task), Some(reason)) =
                (entry.get("task").and_then(Value::as_str), entry.get("reason").and_then(Value::as_str))
            else {
                continue;
            };
            for row in rows_for_task(task) {
                self.disabled.entry(row.into()).or_insert_with(|| reason.into());
            }
        }
    }

    /// The row's status, or `None` if `name` is not a spec key or event.
    pub fn row(&self, name: &str) -> Option<String> {
        if !is_row(name) {
            return None;
        }
        Some(if self.plugin_status.as_deref() == Some("error") {
            format!("error: {}", self.plugin_error.as_deref().unwrap_or("no reason given"))
        } else if let Some(reason) = self.disabled.get(name) {
            format!("disabled: {reason}")
        } else if self.seen.contains(name) {
            "supported".into()
        } else {
            "unavailable".into()
        })
    }

    /// Every row in spec order: keys, then events.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        KEYS.iter()
            .map(|&(_, k)| k)
            .chain(EVENTS)
            .map(|name| (name, self.row(name).unwrap_or_default()))
            .collect()
    }

    /// The collector's latest `plugin_status`, explained.
    pub fn plugin_status(&self) -> String {
        let Some(s) = self.plugin_status.as_deref() else {
            return "no status received".into();
        };
        let what = match s {
            "initializing" => "plugin started, collector not ready yet",
            "initialized" => "roots resolved, main-loop hook not armed yet",
            "scanning" => "collecting",
            "stalled" => "no pass for 5 s: the main-loop call site stopped firing",
            "selftest" => "test mode: no game data is read",
            "discovering" => "main-loop discovery: no game data is read",
            "discovered" => "discovery finished: see gep_internal.discovery",
            "shutdown" => "collector stopped",
            "error" => {
                return format!("error: {}", self.plugin_error.as_deref().unwrap_or("no reason given"));
            }
            _ => "unknown status",
        };
        format!("{s} ({what})")
    }

    pub fn render(&self) -> String {
        let mut out = format!("plugin_status: {}\n", self.plugin_status());
        out.push_str(&format!("{:<6} {:<28} {}\n", "kind", "feature", "status"));
        for &(category, key) in &KEYS {
            let name = format!("{category}.{key}");
            out.push_str(&format!("{:<6} {:<28} {}\n", "info", name, self.row(key).unwrap_or_default()));
        }
        for name in EVENTS {
            out.push_str(&format!("{:<6} {:<28} {}\n", "event", name, self.row(name).unwrap_or_default()));
        }
        out
    }
}
