use uw_spike_dlcollector::mem::MemRead;
use uw_spike_dlcollector::module::{Module, file_image};
use uw_spike_dlcollector::roots::{CLOCK_RULE_FIELDS, resolve_clock_roots, resolve_roots};
use uw_spike_dlcollector::schema::SchemaFields;

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
}
