use uw_spike_dlcollector::mem::{FakeMem, MemRead, ReadError};
use uw_spike_dlcollector::module::{Module, Section};
use uw_spike_dlcollector::pattern::{Pattern, SCAN_CHUNK, rip_target, scan};

fn module_with(sections: Vec<Section>) -> Module {
    Module {
        name: "t.dll".into(),
        base: 0x10_0000,
        image_size: 0x40_0000,
        sections,
    }
}

#[test]
fn pattern_parse_and_scan() {
    let bytes = [0x90, 0x48, 0x8B, 0x05, 1, 2, 3, 4, 0xC3];
    let mut mem = FakeMem::new();
    mem.add(0x10_1000, bytes.to_vec());
    mem.add(0x10_2000, bytes.to_vec());
    let exec = module_with(vec![Section {
        rva: 0x1000,
        size: 9,
        executable: true,
    }]);
    let p = Pattern::parse("48 8B 05 ?? ?? ?? ?? C3").unwrap();
    assert_eq!(scan(&mem, &exec, &p, 16).unwrap(), vec![0x10_1001]);

    assert!(Pattern::parse("4G").is_err());
    assert!(Pattern::parse("").is_err());
    assert!(Pattern::parse("?? ??").is_err());

    let data_only = module_with(vec![Section {
        rva: 0x2000,
        size: 9,
        executable: false,
    }]);
    assert!(scan(&mem, &data_only, &p, 16).unwrap().is_empty());
}

#[test]
fn scan_limit_is_an_error_not_an_unbounded_vec() {
    let mut mem = FakeMem::new();
    mem.add(0x10_1000, vec![0xAA; 64]);
    let m = module_with(vec![Section {
        rva: 0x1000,
        size: 64,
        executable: true,
    }]);
    let p = Pattern::parse("AA").unwrap();
    assert_eq!(scan(&mem, &m, &p, 63), Err(ReadError::LimitExceeded));
    assert_eq!(scan(&mem, &m, &p, 64).unwrap().len(), 64);
}

#[test]
fn scan_finds_match_across_chunk_boundary_once() {
    let size = SCAN_CHUNK * 2;
    let mut data = vec![0u8; size];
    let at = SCAN_CHUNK - 2;
    data[at..at + 4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let mut mem = FakeMem::new();
    mem.add(0x10_1000, data);
    let m = module_with(vec![Section {
        rva: 0x1000,
        size: size as u32,
        executable: true,
    }]);
    let p = Pattern::parse("DE AD ?? EF").unwrap();
    assert_eq!(scan(&mem, &m, &p, 4).unwrap(), vec![0x10_1000 + at as u64]);
}

#[test]
fn rip_target_decodes_signed_displacement() {
    let mut mem = FakeMem::new();
    mem.add(0x1000, vec![0x48, 0x8B, 0x05, 0x10, 0, 0, 0]);
    mem.add(0x2000, vec![0x48, 0x8B, 0x05, 0xF0, 0xFF, 0xFF, 0xFF]);
    assert_eq!(rip_target(&mem, 0x1000, 3, 7).unwrap(), 0x1017);
    assert_eq!(rip_target(&mem, 0x2000, 3, 7).unwrap(), 0x2007 - 0x10);
    assert_eq!(rip_target(&mem, 0x2000, 3, 7).unwrap(), 0x1FF7);
    let m: &dyn MemRead = &mem;
    assert!(rip_target(m, 0x9000, 3, 7).is_err());
}
