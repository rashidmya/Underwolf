#![allow(dead_code)]

//! A fake Deadlock process: game rules, teams, entity pages, controllers and clock globals in
//! one heap region, with the module-level roots pointing at them.

use super::Synth;
use uw_spike_dlcollector::layout::RELATIONS;
use uw_spike_dlcollector::mem::FakeMem;
use uw_spike_dlcollector::module::Module;
use uw_spike_dlcollector::roots::{
    ADJUST_TIME, CLOCK_SECONDS, CONVAR_GETTER, ClockRoots, ENTITIES, GAME_TIME, RULES, Roots, TEAMS,
};
use uw_spike_dlcollector::schema::SchemaFields;

pub const HEAP: u64 = 0x2_0000_0000;
pub const HEAP_SIZE: usize = 0x34000;
pub const RULES_OBJ: u64 = HEAP;
pub const VTABLE: u64 = HEAP + 0xb000;
pub const TEAM_ARRAY: u64 = HEAP + 0xb400;
pub const TEAM_OBJ: u64 = HEAP + 0xc000; // + team * 0x1000
pub const HANDLES: u64 = HEAP + 0xe000; // + team * 0x100
pub const PAGE0: u64 = HEAP + 0x10000;
pub const PAGE_TABLE: u64 = HEAP + 0x1f000;
pub const CONTROLLER: u64 = HEAP + 0x20000; // + n * 0x1000
pub const UPGRADES: u64 = HEAP + 0x30000; // + n * 0x100
pub const CLOCK_GLOBALS: u64 = HEAP + 0x32000;
pub const OFFSET_CONVAR: u64 = HEAP + 0x32200;
pub const PREGAME_CONVAR: u64 = HEAP + 0x32300;
pub const PREGAME_ENABLED: u64 = HEAP + 0x32400;

pub const STRIDE: u64 = 0x70;
pub const SERIAL: u64 = 0x10;
pub const ADJUST_FN: u64 = 0x1_4000_9000;

pub const OFF_STATE: u64 = 0x74;
pub const OFF_MATCH_ID: u64 = 0xa060;
pub const OFF_PAUSED_TICKS: u64 = 0x30;
pub const OFF_PAUSE_START: u64 = 0x34;
pub const OFF_PAUSED: u64 = 0x38;
pub const OFF_START: u64 = 0x60;
pub const OFF_MATCH_MODE: u64 = 0xa8;
pub const OFF_MODE: u64 = 0xac;
pub const OFF_PLAYERS: u64 = 0x5f0;
pub const OFF_TEAM_ID: u64 = 0x3ef;
pub const OFF_NAME: u64 = 0x6fc;
pub const OFF_STEAM: u64 = 0x788;
pub const OFF_LOCAL: u64 = 0x790;
pub const OFF_DATA: u64 = 0x908;
pub const OFF_HERO: u64 = 0x1c;
pub const OFF_KILLS: u64 = 0x58;
pub const OFF_DEATHS: u64 = 0x64;
pub const OFF_ASSISTS: u64 = 0x60;
pub const OFF_UPGRADES: u64 = 0xa0;

#[derive(Clone)]
pub struct Player {
    pub name: String,
    pub steam: u64,
    pub hero: u32,
    pub local: bool,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub items: Vec<u32>,
}

impl Player {
    pub fn new(name: &str, steam: u64, hero: u32) -> Player {
        Player {
            name: name.into(),
            steam,
            hero,
            local: false,
            kills: 0,
            deaths: 0,
            assists: 0,
            items: Vec::new(),
        }
    }

    pub fn local(mut self) -> Player {
        self.local = true;
        self
    }

    pub fn items(mut self, items: &[u32]) -> Player {
        self.items = items.to_vec();
        self
    }
}

/// `n` players per team; names `P<team>_<i>`, steam ids `76561198000000001...`, hero 1+.
pub fn teams_of(sizes: &[usize]) -> Vec<Vec<Player>> {
    let mut steam = 76561198000000001u64;
    let mut hero = 1;
    sizes
        .iter()
        .enumerate()
        .map(|(t, &n)| {
            (0..n)
                .map(|i| {
                    let p = Player::new(&format!("P{t}_{i}"), steam, hero);
                    steam += 1;
                    hero += 1;
                    p
                })
                .collect()
        })
        .collect()
}

pub struct World {
    pub roots: Roots,
    pub clock: ClockRoots,
}

pub fn w32(mem: &mut FakeMem, addr: u64, v: u32) {
    assert!(mem.write(addr, &v.to_le_bytes()), "unmapped {addr:#x}");
}

pub fn w64(mem: &mut FakeMem, addr: u64, v: u64) {
    assert!(mem.write(addr, &v.to_le_bytes()), "unmapped {addr:#x}");
}

pub fn wf(mem: &mut FakeMem, addr: u64, v: f32) {
    w32(mem, addr, v.to_bits());
}

pub fn w8(mem: &mut FakeMem, addr: u64, v: u8) {
    assert!(mem.write(addr, &[v]), "unmapped {addr:#x}");
}

impl World {
    /// Maps the heap into `mem` and writes the module-level pointer globals, so the roots lead
    /// into it. The clock starts at 08:15 (underlock's fixture values) in `GameInProgress`.
    pub fn attach(mem: &mut FakeMem, roots: Roots, clock: ClockRoots) -> World {
        mem.add(HEAP, vec![0; HEAP_SIZE]);
        let w = World { roots, clock };
        w64(mem, w.roots.game_rules_ptr, RULES_OBJ);
        w64(mem, w.clock.globals_ptr, CLOCK_GLOBALS);
        w64(mem, w.clock.offset_convar_ptr, OFFSET_CONVAR);
        w64(mem, w.clock.pregame_convar_ptr, PREGAME_CONVAR);
        w64(mem, w.clock.pregame_enabled_ptr, PREGAME_ENABLED);
        w64(mem, w.roots.entity_list_ptr, PAGE_TABLE);
        w64(mem, PAGE_TABLE, PAGE0);
        w64(mem, RULES_OBJ, VTABLE);
        w64(mem, VTABLE + 0x220, w.clock.adjust_time_fn);
        w32(mem, RULES_OBJ + OFF_STATE, 7);
        w32(mem, RULES_OBJ + OFF_MATCH_ID, 1234);
        wf(mem, CLOCK_GLOBALS + 0x30, 535.055_36);
        wf(mem, CLOCK_GLOBALS + 0x54, 0.03125);
        wf(mem, RULES_OBJ + OFF_START, 39.375);
        wf(mem, PREGAME_CONVAR + 0x58, 40.0);
        w
    }

    /// A heap in a plain region with root globals in `GLOBALS` (no module image).
    pub fn standalone() -> (FakeMem, World) {
        const GLOBALS: u64 = 0x1_0000_0000;
        let mut mem = FakeMem::new();
        mem.add(GLOBALS, vec![0; 0x200]);
        let roots = Roots {
            game_rules_ptr: GLOBALS,
            teams: GLOBALS + 0x10,
            entity_list_ptr: GLOBALS + 0x30,
            entity_stride: STRIDE as u32,
            serial_offset: SERIAL as u32,
        };
        let clock = ClockRoots {
            globals_ptr: GLOBALS + 0x40,
            adjust_time_fn: ADJUST_FN,
            offset_convar_ptr: GLOBALS + 0x48,
            pregame_convar_ptr: GLOBALS + 0x50,
            pregame_enabled_ptr: GLOBALS + 0x58,
        };
        let w = World::attach(&mut mem, roots, clock);
        (mem, w)
    }

    pub fn rules_present(&self, mem: &mut FakeMem, present: bool) {
        w64(
            mem,
            self.roots.game_rules_ptr,
            if present { RULES_OBJ } else { 0 },
        );
    }

    pub fn set_state(&self, mem: &mut FakeMem, state: i32) {
        w32(mem, RULES_OBJ + OFF_STATE, state as u32);
    }

    /// Writes the teams, handle lists, entity entries and controllers. Team `t` gets team id
    /// `2 + t`; the `n`-th player overall gets handle `0x8002 + n`.
    pub fn set_players(&self, mem: &mut FakeMem, teams: &[Vec<Player>]) {
        w32(mem, self.roots.teams, teams.len() as u32);
        w64(mem, self.roots.teams + 8, TEAM_ARRAY);
        let mut n = 0u64;
        for (t, team) in teams.iter().enumerate() {
            let t = t as u64;
            let obj = TEAM_OBJ + t * 0x1000;
            w64(mem, TEAM_ARRAY + t * 8, obj);
            w8(mem, obj + OFF_TEAM_ID, 2 + t as u8);
            w32(mem, obj + OFF_PLAYERS, team.len() as u32);
            w64(mem, obj + OFF_PLAYERS + 8, HANDLES + t * 0x100);
            for (i, p) in team.iter().enumerate() {
                let handle = 0x8002 + n as u32;
                let entry = PAGE0 + (handle as u64 & 0x1ff) * STRIDE;
                let ctrl = CONTROLLER + n * 0x1000;
                w32(mem, HANDLES + t * 0x100 + i as u64 * 4, handle);
                w64(mem, entry, ctrl);
                w32(mem, entry + SERIAL, handle);
                let mut name = p.name.as_bytes().to_vec();
                name.push(0);
                assert!(mem.write(ctrl + OFF_NAME, &name));
                w64(mem, ctrl + OFF_STEAM, p.steam);
                w8(mem, ctrl + OFF_LOCAL, p.local as u8);
                let data = ctrl + OFF_DATA;
                w32(mem, data + OFF_HERO, p.hero);
                w32(mem, data + OFF_KILLS, p.kills);
                w32(mem, data + OFF_DEATHS, p.deaths);
                w32(mem, data + OFF_ASSISTS, p.assists);
                w32(mem, data + OFF_UPGRADES, p.items.len() as u32);
                w64(mem, data + OFF_UPGRADES + 8, UPGRADES + n * 0x100);
                for (k, id) in p.items.iter().enumerate() {
                    w32(mem, UPGRADES + n * 0x100 + k as u64 * 4, *id);
                }
                n += 1;
            }
        }
    }

    /// Address of the handle array of team `t` and of the `n`-th player's controller.
    pub fn handles(t: u64) -> u64 {
        HANDLES + t * 0x100
    }

    pub fn controller(n: u64) -> u64 {
        CONTROLLER + n * 0x1000
    }

    pub fn upgrades_header(n: u64) -> u64 {
        Self::controller(n) + OFF_DATA + OFF_UPGRADES
    }
}

/// A class record with `fields` plus the anchor of each of their relations
/// (`layout::RELATIONS`) at the field's offset minus the delta, so the layout check holds.
/// `missing` names an anchor to leave out.
fn class(s: &mut Synth, name: &str, fields: &[(&'static str, u32)], missing: Option<&str>) -> u64 {
    let mut all = fields.to_vec();
    for r in RELATIONS.iter().filter(|r| r.class == name) {
        let Some(&(_, at)) = fields.iter().find(|(f, _)| *f == r.field) else {
            continue;
        };
        if Some(r.anchor) != missing && !all.iter().any(|(f, _)| *f == r.anchor) {
            all.push((r.anchor, (i64::from(at) - r.delta) as u32));
        }
    }
    s.class(name, &all)
}

/// Class records for every field the tasks read (and their layout anchors), at the offsets of
/// build 6801. `missing_anchor`: see `Skip`.
pub fn add_classes(s: &mut Synth, missing_anchor: Option<&str>) -> u64 {
    let m = missing_anchor;
    let rules = class(
        s,
        "C_CitadelGameRules",
        &[
            ("m_eGameState", OFF_STATE as u32),
            ("m_unMatchID", OFF_MATCH_ID as u32),
            ("m_flGameStartTime", OFF_START as u32),
            ("m_eMatchMode", OFF_MATCH_MODE as u32),
            ("m_eGameMode", OFF_MODE as u32),
        ],
        m,
    );
    let base_rules = class(
        s,
        "C_GameRules",
        &[
            ("m_nTotalPausedTicks", OFF_PAUSED_TICKS as u32),
            ("m_nPauseStartTick", OFF_PAUSE_START as u32),
            ("m_bGamePaused", OFF_PAUSED as u32),
        ],
        m,
    );
    let team = class(
        s,
        "C_Team",
        &[("m_aPlayerControllers", OFF_PLAYERS as u32)],
        m,
    );
    let entity = class(s, "C_BaseEntity", &[("m_iTeamNum", OFF_TEAM_ID as u32)], m);
    let ctrl = class(
        s,
        "CBasePlayerController",
        &[
            ("m_iszPlayerName", OFF_NAME as u32),
            ("m_steamID", OFF_STEAM as u32),
            ("m_bIsLocalPlayerController", OFF_LOCAL as u32),
        ],
        m,
    );
    let citadel = class(
        s,
        "CCitadelPlayerController",
        &[("m_PlayerDataGlobal", OFF_DATA as u32)],
        m,
    );
    let data = class(
        s,
        "PlayerDataGlobal_t",
        &[
            ("m_nHeroID", OFF_HERO as u32),
            ("m_iPlayerKills", OFF_KILLS as u32),
            ("m_iDeaths", OFF_DEATHS as u32),
            ("m_iPlayerAssists", OFF_ASSISTS as u32),
            ("m_vecUpgrades", OFF_UPGRADES as u32),
        ],
        m,
    );
    s.class_table(&[rules, base_rules, team, entity, ctrl, citadel, data])
}

/// Schema fields of build 6801 for the tasks, without a module image.
pub fn fields() -> SchemaFields {
    let mut s = Synth::new();
    let table = add_classes(&mut s, None);
    s.classes_site(0x100, table);
    let (mem, module) = s.finish();
    SchemaFields::load(&mem, &module).unwrap()
}

const MARKER: &str = "%s:  CGameRules::CGameRules constructed\n";
const GAME_TIME_AT: usize = 0x800;
const ADJUST_AT: usize = 0x900;
const GETTER_AT: usize = 0xa00;
const CLOCK_AT: usize = 0xb00;

/// What to leave out of a synthetic client image.
#[derive(Default, Clone, Copy)]
pub struct Skip {
    pub rules: bool,
    pub clock: bool,
    pub roster_class: bool,
    pub match_id_field: bool,
    /// A layout anchor (`layout::RELATIONS`) left out of the schema.
    pub missing_anchor: Option<&'static str>,
}

/// A synthetic `client.dll` with every root pattern and schema class, plus a live heap.
pub fn image(skip: Skip) -> (FakeMem, Module, World) {
    let mut s = Synth::new();
    let rules_g = s.alloc(&[0; 8]);
    let teams_g = s.alloc(&[0; 32]);
    let ents_g = s.alloc(&[0; 8]);
    let clock_g = s.alloc(&[0; 8]);
    let offset = s.alloc(&[0; 16]);
    let pregame = s.alloc(&[0; 16]);
    let enabled = s.alloc(&[0; 8]);

    if skip.match_id_field {
        let rules = class(
            &mut s,
            "C_CitadelGameRules",
            &[("m_eGameState", OFF_STATE as u32)],
            None,
        );
        let table = s.class_table(&[rules]);
        s.classes_site(0x100, table);
    } else if skip.roster_class {
        let rules = class(
            &mut s,
            "C_CitadelGameRules",
            &[
                ("m_eGameState", OFF_STATE as u32),
                ("m_unMatchID", OFF_MATCH_ID as u32),
            ],
            None,
        );
        let table = s.class_table(&[rules]);
        s.classes_site(0x100, table);
    } else {
        let table = add_classes(&mut s, skip.missing_anchor);
        s.classes_site(0x100, table);
    }
    if !skip.rules {
        let m = s.cstr(MARKER);
        s.place(0x200, RULES, &[(3, 7, rules_g), (0x21, 0x25, m)]);
    }
    s.place(0x300, TEAMS, &[(6, 10, teams_g), (15, 19, teams_g + 8)]);
    s.place(0x400, ENTITIES, &[(3, 7, ents_g)]);
    s.poke(0x400 + 48, STRIDE as u8);
    s.poke(0x400 + 56, SERIAL as u8);

    if !skip.clock {
        s.place(GAME_TIME_AT, GAME_TIME, &[(0x16, 0x1a, clock_g)]);
        s.poke(GAME_TIME_AT + 0x1e, 0x40);
        s.place(ADJUST_AT, ADJUST_TIME, &[(0x5f, 0x63, clock_g)]);
        s.place(GETTER_AT, CONVAR_GETTER, &[]);
        let (getter, game_time) = (s.code_va(GETTER_AT), s.code_va(GAME_TIME_AT));
        s.place(
            CLOCK_AT,
            CLOCK_SECONDS,
            &[
                (8, 12, offset),
                (18, 22, getter),
                (25, 29, enabled),
                (53, 57, pregame),
                (58, 62, getter),
                (77, 81, game_time),
            ],
        );
    }
    let adjust = s.code_va(ADJUST_AT);
    let (mut mem, module) = s.finish();
    let roots = Roots {
        game_rules_ptr: rules_g,
        teams: teams_g,
        entity_list_ptr: ents_g,
        entity_stride: STRIDE as u32,
        serial_offset: SERIAL as u32,
    };
    let clock = ClockRoots {
        globals_ptr: clock_g,
        adjust_time_fn: adjust,
        offset_convar_ptr: offset + 8,
        pregame_convar_ptr: pregame + 8,
        pregame_enabled_ptr: enabled,
    };
    let world = World::attach(&mut mem, roots, clock);
    (mem, module, world)
}
