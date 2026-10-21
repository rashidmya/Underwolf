use uw_spike_dlcollector::module::{Module, file_image};
use uw_spike_dlcollector::roots::resolve_roots;
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

    let f = SchemaFields::load(&mem, &client).unwrap();
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
