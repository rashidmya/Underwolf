//! The portable half of the Wine-side helper: turns what it reads from a GEP data channel into
//! the messages it relays to `uw-gep`. Values and event data travel as UTF-8 text; anything else
//! is dropped (and logged), never mangled.

use uw_spike_gep_channel::{EventRecord, InfoChange};
use uw_spike_ipc::Msg;

/// Info changes (a snapshot or a `changed` batch) → `GepInfo`, in order.
pub fn relay(changes: Vec<InfoChange>) -> Vec<Msg> {
    changes
        .into_iter()
        .filter_map(|c| {
            let value = match c.value.map(String::from_utf8).transpose() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("[uw-gep-helper] dropped {}.{}: value is not UTF-8", c.category, c.key);
                    return None;
                }
            };
            Some(Msg::GepInfo { category: c.category, key: c.key, value, ts: c.ts })
        })
        .collect()
}

/// One ring drain → `GepLost` (when records were lost before these) followed by `GepEvent`s.
pub fn relay_events(records: Vec<EventRecord>, lost: u64) -> Vec<Msg> {
    let lost = (lost > 0).then_some(Msg::GepLost { events: lost });
    let events = records.into_iter().filter_map(|r| match String::from_utf8(r.data) {
        Ok(data) => Some(Msg::GepEvent { name: r.name, data, ts: r.ts }),
        Err(_) => {
            eprintln!("[uw-gep-helper] dropped event {}: data is not UTF-8", r.name);
            None
        }
    });
    lost.into_iter().chain(events).collect()
}
