//! Roster and items: the entity-handle walk of underlock's `readDeadlockRoster`, with every
//! vector header, handle list, entity page and entry re-read after use.

use super::{at, json_str, offset, rules_object};
use crate::keys::{ITEMS_MAX, MATCH_INFO, ROSTER_MAX, SLOTS, items_key, roster_key};
use crate::mem::{MemRead, PTR_MAX, PTR_MIN, ReadError};
use crate::roots::Roots;
use crate::runner::{Pass, Task, TaskError};
use crate::schema::SchemaFields;

const MAX_TEAMS: usize = 32;
const MAX_HANDLES: usize = 64;
const MAX_UPGRADES: usize = 64;
const NAME_MAX: usize = 128;
const NO_HANDLE: u32 = 0xffff_ffff;
const PAGE_ENTRIES: u32 = 0x200;
const PAGE_MASK: u32 = 0x3f;

/// Offsets of every field the roster reads, resolved by schema name.
#[derive(Debug, Clone)]
pub struct RosterFields {
    team_players: u32,
    team_id: u32,
    name: u32,
    steam_id: u32,
    local: u32,
    data: u32,
    hero: u32,
    kills: u32,
    deaths: u32,
    assists: u32,
    upgrades: u32,
}

impl RosterFields {
    /// `Err` names the first field that is missing.
    pub fn resolve(f: &SchemaFields) -> Result<RosterFields, String> {
        let get = |class: &str, field: &str| {
            offset(f, class, field).ok_or_else(|| format!("{class}.{field}"))
        };
        Ok(RosterFields {
            team_players: get("C_Team", "m_aPlayerControllers")?,
            team_id: get("C_BaseEntity", "m_iTeamNum")?,
            name: get("CBasePlayerController", "m_iszPlayerName")?,
            steam_id: get("CBasePlayerController", "m_steamID")?,
            local: get("CBasePlayerController", "m_bIsLocalPlayerController")?,
            data: get("CCitadelPlayerController", "m_PlayerDataGlobal")?,
            hero: get("PlayerDataGlobal_t", "m_nHeroID")?,
            kills: get("PlayerDataGlobal_t", "m_iPlayerKills")?,
            deaths: get("PlayerDataGlobal_t", "m_iDeaths")?,
            assists: get("PlayerDataGlobal_t", "m_iPlayerAssists")?,
            upgrades: get("PlayerDataGlobal_t", "m_vecUpgrades")?,
        })
    }
}

struct Player {
    name: String,
    steam_id: String,
    hero: u32,
    team: u8,
    local: bool,
    kills: u32,
    deaths: u32,
    assists: u32,
    items: Vec<u32>,
}

impl Player {
    fn identity(&self) -> (String, String) {
        (self.name.clone(), self.steam_id.clone())
    }

    fn roster_json(&self) -> String {
        format!(
            "{{\"player_name\":{},\"steam_id\":\"{}\",\"hero_id\":{},\"team_id\":{},\"is_local\":{},\"kills\":{},\"deaths\":{},\"assists\":{}}}",
            json_str(&self.name),
            self.steam_id,
            self.hero,
            self.team,
            self.local,
            self.kills,
            self.deaths,
            self.assists
        )
    }

    fn items_json(&self) -> String {
        let items: Vec<String> = self
            .items
            .iter()
            .map(|id| format!("{{\"id\":{id}}}"))
            .collect();
        format!(
            "{{\"player_name\":{},\"steam_id\":\"{}\",\"items\":[{}]}}",
            json_str(&self.name),
            self.steam_id,
            items.join(",")
        )
    }
}

/// A `{count: i32, data: pointer}` vector header, as read.
struct VecHeader {
    addr: u64,
    raw: [u8; 16],
    count: usize,
    data: u64,
}

impl VecHeader {
    fn read(m: &dyn MemRead, addr: u64, max: usize) -> Result<VecHeader, TaskError> {
        let mut raw = [0u8; 16];
        m.read(addr, &mut raw)?;
        let count = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let data = u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]);
        let count = usize::try_from(count)
            .ok()
            .filter(|&c| c <= max)
            .ok_or(ReadError::LimitExceeded)?;
        if count > 0 && !(PTR_MIN..=PTR_MAX).contains(&data) {
            return Err(ReadError::Implausible(data).into());
        }
        Ok(VecHeader {
            addr,
            raw,
            count,
            data,
        })
    }

    fn unchanged(&self, m: &dyn MemRead) -> Result<(), TaskError> {
        let mut raw = [0u8; 16];
        m.read(self.addr, &mut raw)?;
        ensure(raw == self.raw)
    }

    fn elements(&self, m: &dyn MemRead, size: usize) -> Result<Vec<u8>, TaskError> {
        read_bytes(m, self.data, self.count * size)
    }
}

fn ensure(same: bool) -> Result<(), TaskError> {
    if same { Ok(()) } else { Err(TaskError::Torn) }
}

fn read_bytes(m: &dyn MemRead, addr: u64, len: usize) -> Result<Vec<u8>, TaskError> {
    let mut buf = vec![0u8; len];
    if len > 0 {
        m.read(addr, &mut buf)?;
    }
    Ok(buf)
}

/// A pointer array element: null is `None`, an implausible value is an error.
fn pointer(raw: &[u8]) -> Result<Option<u64>, TaskError> {
    let mut b = [0u8; 8];
    b.copy_from_slice(raw);
    match u64::from_le_bytes(b) {
        0 => Ok(None),
        p if (PTR_MIN..=PTR_MAX).contains(&p) => Ok(Some(p)),
        p => Err(ReadError::Implausible(p).into()),
    }
}

fn u32s(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub struct Roster {
    teams: u64,
    entity_list_ptr: u64,
    rules_ptr: u64,
    stride: u64,
    serial_offset: u64,
    f: RosterFields,
    /// Identity held by each slot; changed only by a pass that succeeded.
    slots: [Option<(String, String)>; SLOTS],
}

impl Roster {
    pub fn new(roots: &Roots, f: RosterFields) -> Roster {
        Roster {
            teams: roots.teams,
            entity_list_ptr: roots.entity_list_ptr,
            rules_ptr: roots.game_rules_ptr,
            stride: roots.entity_stride.into(),
            serial_offset: roots.serial_offset.into(),
            f,
            slots: Default::default(),
        }
    }

    fn read_players(&self, m: &dyn MemRead) -> Result<Vec<Player>, TaskError> {
        let f = &self.f;
        let teams = VecHeader::read(m, self.teams, MAX_TEAMS)?;
        let list_root = m.read_u64(self.entity_list_ptr)?;
        let entity_list = pointer(&list_root.to_le_bytes())?;
        if teams.count > 0 && entity_list.is_none() {
            // The entity list is not set up yet.
            return Err(TaskError::Torn);
        }
        let team_bytes = teams.elements(m, 8)?;
        let mut players: Vec<Player> = Vec::new();
        for team_raw in team_bytes.chunks_exact(8) {
            let Some(team) = pointer(team_raw)? else {
                continue;
            };
            let team_id = m.read_u8(at(team, f.team_id.into())?)?;
            let handles = VecHeader::read(m, at(team, f.team_players.into())?, MAX_HANDLES)?;
            let handle_bytes = handles.elements(m, 4)?;
            for handle in u32s(&handle_bytes) {
                if handle == NO_HANDLE {
                    continue;
                }
                let Some(list) = entity_list else {
                    continue;
                };
                if let Some(player) = self.read_player(m, list, handle, team_id)? {
                    players.push(player);
                }
            }
            handles.unchanged(m)?;
            ensure(
                handles.count == 0
                    || read_bytes(m, handles.data, handle_bytes.len())? == handle_bytes,
            )?;
        }
        teams.unchanged(m)?;
        ensure(teams.count == 0 || read_bytes(m, teams.data, team_bytes.len())? == team_bytes)?;
        ensure(m.read_u64(self.entity_list_ptr)? == list_root)?;

        if players.iter().filter(|p| p.local).count() > 1 {
            return Err(TaskError::Invalid("more than one local player".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !players.iter().all(|p| seen.insert(p.identity())) {
            return Err(TaskError::Invalid("duplicate player identity".into()));
        }
        if players.len() > SLOTS {
            return Err(TaskError::Invalid(format!(
                "more than {SLOTS} roster entries"
            )));
        }
        Ok(players)
    }

    /// One handle's player; `None` for a stale handle, a null controller or an unset hero.
    fn read_player(
        &self,
        m: &dyn MemRead,
        list: u64,
        handle: u32,
        team: u8,
    ) -> Result<Option<Player>, TaskError> {
        let f = &self.f;
        let page_addr = at(list, u64::from((handle >> 9) & PAGE_MASK) * 8)?;
        let page_raw = read_bytes(m, page_addr, 8)?;
        let Some(page) = pointer(&page_raw)? else {
            return Ok(None);
        };
        let entry = at(page, u64::from(handle % PAGE_ENTRIES) * self.stride)?;
        let first_len = usize::try_from(self.serial_offset + 4)
            .map_err(|_| ReadError::LimitExceeded)?
            .max(8);
        let first = read_bytes(m, entry, first_len)?;
        let serial_at =
            usize::try_from(self.serial_offset).map_err(|_| ReadError::LimitExceeded)?;
        if u32s(&first[serial_at..serial_at + 4])[0] != handle {
            return Ok(None);
        }
        let Some(controller) = pointer(&first[..8])? else {
            return Ok(None);
        };

        let name = m.read_cstr(at(controller, f.name.into())?, NAME_MAX)?;
        let steam_id = m.read_u64(at(controller, f.steam_id.into())?)?.to_string();
        let data = at(controller, f.data.into())?;
        let hero = m.read_u32(at(data, f.hero.into())?)?;
        let local_at = at(controller, f.local.into())?;
        let flag = m.read_u8(local_at)?;
        let kills = m.read_u32(at(data, f.kills.into())?)?;
        let deaths = m.read_u32(at(data, f.deaths.into())?)?;
        let assists = m.read_u32(at(data, f.assists.into())?)?;
        let upgrades = VecHeader::read(m, at(data, f.upgrades.into())?, MAX_UPGRADES)?;
        let item_bytes = upgrades.elements(m, 4)?;
        upgrades.unchanged(m)?;
        ensure(
            upgrades.count == 0 || read_bytes(m, upgrades.data, item_bytes.len())? == item_bytes,
        )?;

        ensure(read_bytes(m, entry, first.len())? == first)?;
        ensure(read_bytes(m, page_addr, 8)? == page_raw)?;
        ensure(m.read_u8(local_at)? == flag)?;
        // Only now, with the entry known stable, is a bad flag a layout problem.
        if flag > 1 {
            return Err(TaskError::Invalid(
                "local controller flag is not 0 or 1".into(),
            ));
        }
        // Unselected heroes are not a roster entry.
        if hero == 0 {
            return Ok(None);
        }
        Ok(Some(Player {
            name,
            steam_id,
            hero,
            team,
            local: flag == 1,
            kills,
            deaths,
            assists,
            items: u32s(&item_bytes),
        }))
    }

    fn clear_all(&mut self, out: &mut Pass) {
        self.slots = Default::default();
        for i in 0..SLOTS {
            out.set(MATCH_INFO, &roster_key(i), None);
            out.set(MATCH_INFO, &items_key(i), None);
        }
    }
}

impl Task for Roster {
    fn name(&self) -> &'static str {
        "roster"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        if rules_object(m, self.rules_ptr)?.is_none() {
            self.clear_all(out);
            return Ok(());
        }
        let players = self.read_players(m)?;

        // Identities that left give up their slot; newcomers take the lowest free one.
        let mut slots = self.slots.clone();
        for slot in &mut slots {
            if slot
                .as_ref()
                .is_some_and(|id| !players.iter().any(|p| &p.identity() == id))
            {
                *slot = None;
            }
        }
        let mut placed: [Option<&Player>; SLOTS] = Default::default();
        for p in &players {
            let id = p.identity();
            let i = match slots.iter().position(|s| s.as_ref() == Some(&id)) {
                Some(i) => i,
                None => {
                    let free = slots
                        .iter()
                        .position(Option::is_none)
                        .ok_or_else(|| TaskError::Invalid("no free roster slot".into()))?;
                    slots[free] = Some(id);
                    free
                }
            };
            placed[i] = Some(p);
        }

        let mut staged = Vec::with_capacity(SLOTS * 2);
        for (i, p) in placed.iter().enumerate() {
            let values = p.map(|p| (p.roster_json(), p.items_json()));
            if let Some((r, it)) = &values
                && (r.len() > ROSTER_MAX as usize || it.len() > ITEMS_MAX as usize)
            {
                return Err(TaskError::Invalid(format!(
                    "slot {i} value is longer than its key allows"
                )));
            }
            staged.push((roster_key(i), values.as_ref().map(|v| v.0.clone())));
            staged.push((items_key(i), values.map(|v| v.1)));
        }
        for (key, value) in staged {
            out.set(MATCH_INFO, &key, value);
        }
        self.slots = slots;
        Ok(())
    }
}
