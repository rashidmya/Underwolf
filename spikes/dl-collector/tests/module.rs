use uw_spike_dlcollector::mem::{MemRead, ReadError};
use uw_spike_dlcollector::module::{Module, file_image};

const IMAGE_BASE: u64 = 0x1_8000_0000;

/// A minimal PE64: headers, `.text` (executable, raw 0x200) and `.rdata` (virtual 0x300, raw 0x200).
fn pe_bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x400 + 0x200 + 0x200];
    b[0..2].copy_from_slice(b"MZ");
    let pe = 0x80usize;
    b[0x3c..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
    b[pe..pe + 4].copy_from_slice(b"PE\0\0");
    b[pe + 4..pe + 6].copy_from_slice(&0x8664u16.to_le_bytes());
    b[pe + 6..pe + 8].copy_from_slice(&2u16.to_le_bytes());
    b[pe + 20..pe + 22].copy_from_slice(&240u16.to_le_bytes());
    let opt = pe + 24;
    b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    b[opt + 24..opt + 32].copy_from_slice(&IMAGE_BASE.to_le_bytes());
    b[opt + 56..opt + 60].copy_from_slice(&0x4000u32.to_le_bytes());
    b[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes());
    let sec = opt + 240;
    let mut section = |i: usize, name: &[u8], vsize: u32, rva: u32, raw: u32, ptr: u32, ch: u32| {
        let s = sec + i * 40;
        b[s..s + name.len()].copy_from_slice(name);
        b[s + 8..s + 12].copy_from_slice(&vsize.to_le_bytes());
        b[s + 12..s + 16].copy_from_slice(&rva.to_le_bytes());
        b[s + 16..s + 20].copy_from_slice(&raw.to_le_bytes());
        b[s + 20..s + 24].copy_from_slice(&ptr.to_le_bytes());
        b[s + 36..s + 40].copy_from_slice(&ch.to_le_bytes());
    };
    section(0, b".text", 0x200, 0x1000, 0x200, 0x400, 0x6000_0020);
    section(1, b".rdata", 0x300, 0x2000, 0x200, 0x600, 0x4000_0040);
    b[0x400..0x404].copy_from_slice(&[0x48, 0x8b, 0x05, 0xc3]);
    b[0x600..0x606].copy_from_slice(b"hello\0");
    b
}

#[test]
fn file_image_maps_sections_at_preferred_base() {
    let (mem, base) = file_image(&pe_bytes()).unwrap();
    assert_eq!(base, IMAGE_BASE);
    let m: &dyn MemRead = &mem;
    assert_eq!(m.read_u8(base + 0x1000).unwrap(), 0x48);
    assert_eq!(m.read_cstr(base + 0x2000, 16).unwrap(), "hello");
    // Virtual size past the raw data reads as zeros, like a loaded image.
    assert_eq!(m.read_u8(base + 0x2000 + 0x2ff).unwrap(), 0);
    assert!(m.read_u8(base + 0x2000 + 0x300).is_err());
    assert_eq!(m.read_u8(base).unwrap(), b'M');
}

#[test]
fn from_headers_parses_sections_from_memory() {
    let (mem, base) = file_image(&pe_bytes()).unwrap();
    let module = Module::from_headers(&mem, "client.dll", base).unwrap();
    assert_eq!(module.name, "client.dll");
    assert_eq!(module.base, IMAGE_BASE);
    assert_eq!(module.image_size, 0x4000);
    assert_eq!(module.sections.len(), 2);
    assert_eq!(
        (
            module.sections[0].rva,
            module.sections[0].size,
            module.sections[0].executable
        ),
        (0x1000, 0x200, true)
    );
    assert_eq!(
        (
            module.sections[1].rva,
            module.sections[1].size,
            module.sections[1].executable
        ),
        (0x2000, 0x300, false)
    );
}

#[test]
fn malformed_images_are_errors() {
    assert!(file_image(&[0u8; 16]).is_err());
    let mut bad_magic = pe_bytes();
    bad_magic[0] = b'X';
    assert!(file_image(&bad_magic).is_err());
    let mut machine = pe_bytes();
    machine[0x80 + 4] = 0x4c;
    assert!(file_image(&machine).is_err());
    let mut too_many = pe_bytes();
    too_many[0x80 + 6] = 200;
    assert!(file_image(&too_many).is_err());
    let mut truncated = pe_bytes();
    truncated.truncate(0x500);
    assert!(matches!(
        file_image(&truncated),
        Err(ReadError::BadImage(_))
    ));
    let mut oversized = pe_bytes();
    oversized[0x80 + 24 + 56..0x80 + 24 + 60].copy_from_slice(&0x1000u32.to_le_bytes());
    assert!(file_image(&oversized).is_err());
}
