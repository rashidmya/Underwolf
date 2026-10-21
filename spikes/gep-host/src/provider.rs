//! The Deadlock event provider, above the transport: turns the collector's info keys and events
//! into Overwolf-style info updates `{feature, category, key, value}` and events `{name, data}`.
//!
//! Info passes through (feature = category). Events come from what the collector read, never
//! from estimates: the local roster row's kills/deaths/assists going up by one (within one match
//! id), the phase entering and leaving a match, and the collector's own events.

use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emission {
    Info { feature: String, category: String, key: String, value: Option<String> },
    Event { name: String, data: String },
}

const GAME_INFO: &str = "game_info";
const MATCH_INFO: &str = "match_info";
const IN_PROGRESS: &str = "GameInProgress";
/// Phases during which a started match has not ended yet.
const MATCH_PHASES: [&str; 3] = [IN_PROGRESS, "PostGame", "PostGame_PlayOfTheGame"];
/// Roster counters and the event each +1 emits, in roster-row field order.
const COUNTERS: [(&str, &str); 3] = [("kills", "kill"), ("deaths", "death"), ("assists", "assist")];

/// The local player's counters, valid within the match id they were read in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Local {
    steam_id: String,
    counts: [u64; 3],
}

#[derive(Debug, Default)]
pub struct Provider {
    phase: Option<String>,
    match_id: Option<String>,
    /// Match ids that already had their `match_start`.
    started: HashSet<String>,
    /// The match that started and has not ended.
    in_match: Option<String>,
    local: Option<Local>,
    steam_id: Option<String>,
}

fn event(name: &str, data: String) -> Emission {
    Emission::Event { name: name.into(), data }
}

fn json_str(s: &str) -> String {
    Value::String(s.into()).to_string()
}

impl Provider {
    pub fn on_info(&mut self, category: &str, key: &str, value: Option<&str>) -> Vec<Emission> {
        let mut out = vec![Emission::Info {
            feature: category.into(),
            category: category.into(),
            key: key.into(),
            value: value.map(Into::into),
        }];
        match (category, key) {
            (GAME_INFO, "phase") => {
                self.phase = value.map(Into::into);
                self.check_end(&mut out);
                self.check_start(&mut out);
            }
            (MATCH_INFO, "match_id") => {
                if self.match_id.as_deref() != value {
                    self.match_id = value.map(Into::into);
                    // Counters are only compared within one match.
                    self.local = None;
                }
                self.check_start(&mut out);
            }
            (MATCH_INFO, k) if k.starts_with("roster_") => {
                if let Some(row) = value {
                    self.on_roster_row(row, &mut out);
                }
            }
            _ => {}
        }
        out
    }

    pub fn on_event(&mut self, name: &str, data: &str) -> Vec<Emission> {
        let name = match name {
            "clock" => "match_clock",
            other => other,
        };
        vec![event(name, data.into())]
    }

    fn check_end(&mut self, out: &mut Vec<Emission>) {
        let in_match_phase = self.phase.as_deref().is_some_and(|p| MATCH_PHASES.contains(&p));
        if !in_match_phase && let Some(id) = self.in_match.take() {
            out.push(event("match_end", format!("{{\"match_id\":{}}}", json_str(&id))));
        }
    }

    /// `match_start` once per match id, when the phase is in progress and the id is known (in one
    /// transaction `phase` arrives before `match_id`, so both orders are handled).
    fn check_start(&mut self, out: &mut Vec<Emission>) {
        if self.phase.as_deref() != Some(IN_PROGRESS) {
            return;
        }
        let Some(id) = self.match_id.clone() else { return };
        if self.started.contains(&id) {
            return;
        }
        if let Some(prev) = self.in_match.take() {
            out.push(event("match_end", format!("{{\"match_id\":{}}}", json_str(&prev))));
        }
        self.started.insert(id.clone());
        out.push(event("match_start", format!("{{\"match_id\":{}}}", json_str(&id))));
        self.in_match = Some(id);
    }

    fn on_roster_row(&mut self, row: &str, out: &mut Vec<Emission>) {
        let Ok(v) = serde_json::from_str::<Value>(row) else {
            eprintln!("[uw-gep] roster row is not JSON: {row}");
            return;
        };
        if v.get("is_local").and_then(Value::as_bool) != Some(true) {
            return;
        }
        let Some(steam_id) = v.get("steam_id").and_then(Value::as_str) else { return };
        if self.steam_id.as_deref() != Some(steam_id) {
            self.steam_id = Some(steam_id.into());
            out.push(Emission::Info {
                feature: GAME_INFO.into(),
                category: GAME_INFO.into(),
                key: "steam_id".into(),
                value: Some(steam_id.into()),
            });
        }
        let mut counts = [0u64; 3];
        for (slot, (field, _)) in counts.iter_mut().zip(COUNTERS) {
            let Some(n) = v.get(field).and_then(Value::as_u64) else { return };
            *slot = n;
        }
        if let Some(prev) = &self.local
            && prev.steam_id == steam_id
        {
            for ((&now, &before), (field, name)) in counts.iter().zip(&prev.counts).zip(COUNTERS) {
                if before.checked_add(1) == Some(now) {
                    out.push(event(name, format!("{{\"{field}\":{now}}}")));
                }
            }
        }
        self.local = Some(Local { steam_id: steam_id.into(), counts });
    }
}
