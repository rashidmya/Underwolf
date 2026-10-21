use std::cell::RefCell;
use std::rc::Rc;

use uw_spike_dlcollector::collector::{ChannelOut, Collector, Status};
use uw_spike_dlcollector::layout::{RELATIONS, holds};
use uw_spike_dlcollector::mem::{FakeMem, MemRead};
use uw_spike_dlcollector::module::{Module, file_image};
use uw_spike_dlcollector::roots::{
    CLOCK_RULE_FIELDS, MAIN_LOOP_IMMEDIATE, MAIN_LOOP_OUTER, resolve_caller, resolve_clock_roots,
    resolve_roots,
};
use uw_spike_dlcollector::schema::SchemaFields;
use uw_spike_dlcollector::tasks::{GAME_MODES, MATCH_MODES, TEAM_AMBER, TEAM_SAPPHIRE};

const DEFAULT_PATH: &str =
    "/home/buga/.local/share/Steam/steamapps/common/Deadlock/game/citadel/bin/win64/client.dll";

/// Reads the installed client.dll from disk (read-only). Run with `-- --ignored installed_build`.
#[test]
#[ignore]
fn installed_build() {
    let path = std::env::var("UW_DEADLOCK_CLIENT_DLL").unwrap_or_else(|_| DEFAULT_PATH.to_string());
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (mem, base) = file_image(&bytes).unwrap();
    drop(bytes);
    let client = Module::from_headers(&mem, "client.dll", base).unwrap();

    let roots = resolve_roots(&mem, &client).unwrap();
    for addr in [roots.game_rules_ptr, roots.teams, roots.entity_list_ptr] {
        assert!(client.contains(addr), "{addr:#x} outside image");
    }
    assert_eq!((roots.entity_stride, roots.serial_offset), (0x70, 0x10));

    // Each clock root resolves to exactly one address (Ambiguous/NotFound otherwise).
    let clock = resolve_clock_roots(&mem, &client).unwrap();
    let in_data = |addr: u64| {
        client.sections.iter().any(|s| {
            !s.executable
                && addr >= base + s.rva as u64
                && addr < base + s.rva as u64 + s.size as u64
        })
    };
    let ptrs = [
        clock.globals_ptr,
        clock.offset_convar_ptr,
        clock.pregame_convar_ptr,
        clock.pregame_enabled_ptr,
    ];
    for addr in ptrs {
        assert!(in_data(addr), "{addr:#x} not in a data section");
    }
    let mut distinct = ptrs.to_vec();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "{clock:?}");
    assert!(client.contains(clock.adjust_time_fn) && !in_data(clock.adjust_time_fn));
    let mut prologue = [0u8; 10];
    mem.read(clock.adjust_time_fn, &mut prologue).unwrap();
    assert_eq!(
        prologue,
        [0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x30]
    );

    let f = SchemaFields::load(&mem, &client).unwrap();
    // Offsets underlock's live-validated clock reader used on the previous build.
    let clock_offsets = [0x30, 0x34, 0x38, 0x60, 0xac];
    for ((role, class, field), off) in CLOCK_RULE_FIELDS.iter().zip(clock_offsets) {
        assert_eq!(f.get(class, field), Some(off), "{role}: {class}.{field}");
    }
    assert_eq!(f.layout_name(), "disk-class-v2");
    let expect = [
        ("C_CitadelGameRules", "m_eGameState", 0x74),
        ("C_CitadelGameRules", "m_unMatchID", 0xa060),
        ("C_CitadelGameRules", "m_eMatchMode", 0xa8),
        ("C_CitadelGameRules", "m_iWinningTeam", 0xc0),
        ("C_Team", "m_iScore", 0x620),
        ("C_CitadelGameRules", "m_flMatchClockAtLastUpdate", 0x9fdc),
        ("C_Team", "m_aPlayerControllers", 0x5f0),
        ("C_BaseEntity", "m_iTeamNum", 0x3ef),
        ("CBasePlayerController", "m_iszPlayerName", 0x6fc),
        ("CBasePlayerController", "m_steamID", 0x788),
        ("CBasePlayerController", "m_bIsLocalPlayerController", 0x790),
        ("CCitadelPlayerController", "m_PlayerDataGlobal", 0x908),
        ("PlayerDataGlobal_t", "m_nHeroID", 0x1c),
        ("PlayerDataGlobal_t", "m_vecUpgrades", 0xa0),
    ];
    for (class, field, off) in expect {
        assert_eq!(
            f.get(class, field),
            Some(off),
            "{class}.{field} (ambiguous: {:?})",
            f.ambiguous()
        );
    }
    // The layout relations of build 6801 hold on this build too.
    for r in RELATIONS {
        assert!(
            holds(&f, r.class, r.field),
            "layout: {}.{}",
            r.class,
            r.field
        );
    }

    // game_mode's names and numbers are this build's own schema enums.
    let named = |table: &[&str], prefix: &str| -> Vec<(String, i64)> {
        (0..)
            .zip(table)
            .map(|(i, n)| (format!("{prefix}{n}"), i))
            .collect()
    };
    assert_eq!(
        schema_enum(&mem, &client, "ECitadelMatchMode"),
        named(&MATCH_MODES, "k_ECitadelMatchMode_")
    );
    assert_eq!(
        schema_enum(&mem, &client, "ECitadelGameMode"),
        named(&GAME_MODES, "k_ECitadelGameMode_")
    );

    // team_score's and match_outcome's team numbers are this build's own.
    assert_eq!(
        team_parser_numbers(&mem, &client, &["TEAM_AMBER", "TEAM_SAPPHIRE"]),
        [u32::from(TEAM_AMBER), u32::from(TEAM_SAPPHIRE)]
    );
}

/// The team number the game's team-name parser stores for each of `names`. Each name has one
/// `lea rdx,[rip+name]` in code, followed by `cmp rbx,rdx; je` to `mov dword [rdi],<number>`.
fn team_parser_numbers(mem: &dyn MemRead, client: &Module, names: &[&str]) -> Vec<u32> {
    let strings: Vec<u64> = names
        .iter()
        .map(|name| {
            let found = find_in_data(mem, client, format!("\0{name}\0").as_bytes());
            assert_eq!(found.len(), 1, "{name}: strings {found:x?}");
            found[0] + 1
        })
        .collect();
    let mut sites = vec![Vec::new(); names.len()];
    for s in client.sections.iter().filter(|s| s.executable) {
        let start = client.base + u64::from(s.rva);
        let mut code = vec![0; s.size as usize];
        mem.read(start, &mut code).unwrap();
        for (i, w) in code.windows(7).enumerate() {
            if w[..3] != [0x48, 0x8d, 0x15] {
                continue;
            }
            let next = start + i as u64 + 7;
            let target =
                next.wrapping_add_signed(i32::from_le_bytes([w[3], w[4], w[5], w[6]]).into());
            if let Some(n) = strings.iter().position(|&a| a == target) {
                sites[n].push(next);
            }
        }
    }
    names
        .iter()
        .zip(sites)
        .map(|(name, sites)| {
            assert_eq!(sites.len(), 1, "{name}: lea sites {sites:x?}");
            let mut cmp_je = [0u8; 5];
            mem.read(sites[0], &mut cmp_je).unwrap();
            assert_eq!(cmp_je[..4], [0x48, 0x3b, 0xda, 0x74], "{name}");
            let store = (sites[0] + 5).wrapping_add_signed((cmp_je[4] as i8).into());
            let mut mov = [0u8; 6];
            mem.read(store, &mut mov).unwrap();
            assert_eq!(mov[..2], [0xc7, 0x07], "{name}");
            u32::from_le_bytes([mov[2], mov[3], mov[4], mov[5]])
        })
        .collect()
}

/// Every address in `client`'s data sections where `pat` starts.
fn find_in_data(mem: &dyn MemRead, client: &Module, pat: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    for s in client.sections.iter().filter(|s| !s.executable) {
        let start = client.base + u64::from(s.rva);
        let mut buf = vec![0; s.size as usize];
        mem.read(start, &mut buf).unwrap();
        out.extend(
            buf.windows(pat.len())
                .enumerate()
                .filter(|(_, w)| *w == pat)
                .map(|(i, _)| start + i as u64),
        );
    }
    out
}

/// The enumerators `(name, value)` of the one schema enum record named `name`: its name
/// pointer at +8, enumerator count (u16) at +0x1c, enumerator table at +0x20; each enumerator
/// is 0x20 bytes, name pointer at +0 and value (i64) at +8.
fn schema_enum(mem: &dyn MemRead, client: &Module, name: &str) -> Vec<(String, i64)> {
    let mut records = Vec::new();
    for at in find_in_data(mem, client, format!("\0{name}\0").as_bytes()) {
        let string = at + 1;
        records.extend(
            find_in_data(mem, client, &string.to_le_bytes())
                .into_iter()
                .map(|p| p - 8),
        );
    }
    assert_eq!(records.len(), 1, "{name}: records {records:x?}");
    let record = records[0];
    let count = mem.read_u16(record + 0x1c).unwrap();
    assert!((1..=256).contains(&count), "{name}: {count} enumerators");
    let table = mem.read_ptr(record + 0x20).unwrap();
    (0..u64::from(count))
        .map(|i| {
            let e = table + i * 0x20;
            let n = mem.read_cstr(mem.read_ptr(e).unwrap(), 128).unwrap();
            (n, mem.read_u64(e + 8).unwrap() as i64)
        })
        .collect()
}

const DEFAULT_BIN_DIR: &str =
    "/home/buga/.local/share/Steam/steamapps/common/Deadlock/game/bin/win64";

/// Maps `<UW_DEADLOCK_BIN_DIR>/<name>` (read-only) as its own image.
fn bin_module(name: &str) -> (FakeMem, Module) {
    let dir = std::env::var("UW_DEADLOCK_BIN_DIR").unwrap_or_else(|_| DEFAULT_BIN_DIR.to_string());
    let path = format!("{dir}/{name}");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (mem, base) = file_image(&bytes).unwrap();
    let module = Module::from_headers(&mem, name, base).unwrap();
    (mem, module)
}

/// Each main-loop pattern has exactly one hit in its installed module, at the return address
/// the discovery run saw. tier0.dll and engine2.dll share a preferred ImageBase, so each is
/// resolved in its own image.
#[test]
#[ignore]
fn installed_build_main_loop_callers() {
    let (tier0_mem, tier0) = bin_module("tier0.dll");
    let f = resolve_caller(
        &tier0_mem,
        std::slice::from_ref(&tier0),
        MAIN_LOOP_IMMEDIATE.as_ref(),
        None,
    )
    .unwrap();
    assert_eq!(f.immediate, tier0.base + 0x14222e);

    let (engine2_mem, engine2) = bin_module("engine2.dll");
    let f = resolve_caller(
        &engine2_mem,
        std::slice::from_ref(&engine2),
        MAIN_LOOP_OUTER.as_ref(),
        None,
    )
    .unwrap();
    assert_eq!(f.immediate, engine2.base + 0x208a73);
}

struct Recorder(Rc<RefCell<Vec<(String, String, Option<Vec<u8>>)>>>);

impl ChannelOut for Recorder {
    fn commit(&mut self, sets: &[(String, String, Option<Vec<u8>>)], _ts: u64) {
        self.0.borrow_mut().extend_from_slice(sets);
    }

    fn push_event(&mut self, _name: &str, _data: &[u8], _ts: u64) -> bool {
        true
    }
}

/// The collector resolves every root, field and task on the installed build, with nothing
/// disabled, and a pass over the (not running) file image publishes nothing but status.
#[test]
#[ignore]
fn installed_build_collector_starts_with_every_task() {
    let path = std::env::var("UW_DEADLOCK_CLIENT_DLL").unwrap_or_else(|_| DEFAULT_PATH.to_string());
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (mem, base) = file_image(&bytes).unwrap();
    drop(bytes);
    let client = Module::from_headers(&mem, "client.dll", base).unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut c = Collector::start(
        &mem,
        &client,
        Box::new(Recorder(log.clone())),
        "",
        Box::new(|| 0),
        1000,
    );
    assert_eq!(c.status(), &Status::Initialized);
    c.tick(&mem);
    assert_eq!(c.status(), &Status::Scanning);
    let value = |key: &str| {
        log.borrow()
            .iter()
            .rev()
            .find(|(_, k, _)| k == key)
            .and_then(|(_, _, v)| v.clone())
            .map(|v| String::from_utf8(v).unwrap())
    };
    assert_eq!(value("disabled_tasks").as_deref(), Some("[]"));
    assert!(value("version_info").unwrap().contains("disk-class-v2"));
    // A file image's game-rules global is zero: nothing is loaded.
    assert!(value("phase").is_none() && value("match_id").is_none());
    assert!(value("game_mode").is_none());
    assert!(value("team_score").is_none() && value("match_outcome").is_none());
}
