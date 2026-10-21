use uw_spike_dlcollector::mem::{FakeMem, MemRead, ReadError};

#[test]
fn read_unmapped_is_error_not_fault() {
    let mut mem = FakeMem::new();
    mem.add(0x1000, 0x10u64.to_le_bytes().to_vec());
    let m: &dyn MemRead = &mem;
    let mut b = [0u8; 4];
    assert!(m.read(0x8, &mut b).is_err());
    assert!(m.read_u32(0x8).is_err());
    assert_eq!(m.read_ptr(0x1000), Err(ReadError::Implausible(0x10)));
    assert_eq!(m.read_u64(0x1000).unwrap(), 0x10);
    // A read that starts mapped but runs past the end is an error, never a partial success.
    assert!(m.read_u64(0x1004).is_err());
}

#[test]
fn read_ptr_accepts_only_plausible_range() {
    let mut mem = FakeMem::new();
    let vals = [0x1000u64, 0x7fff_ffff_ffff, 0x8000_0000_0000, 0xfff, 0];
    let mut b = Vec::new();
    for v in vals {
        b.extend_from_slice(&v.to_le_bytes());
    }
    mem.add(0x2000, b);
    let m: &dyn MemRead = &mem;
    assert_eq!(m.read_ptr(0x2000), Ok(0x1000));
    assert_eq!(m.read_ptr(0x2008), Ok(0x7fff_ffff_ffff));
    assert!(m.read_ptr(0x2010).is_err());
    assert!(m.read_ptr(0x2018).is_err());
    assert!(m.read_ptr(0x2020).is_err());
}

#[test]
fn typed_reads_are_little_endian_and_stitch_regions() {
    let mut mem = FakeMem::new();
    mem.add(0x3000, vec![0x7f, 0x01]);
    mem.add(0x3002, vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    let f = 1.5f32.to_le_bytes();
    mem.add(0x4000, f.to_vec());
    mem.add(0x4004, (-5i32).to_le_bytes().to_vec());
    let m: &dyn MemRead = &mem;
    assert_eq!(m.read_u8(0x3000).unwrap(), 0x7f);
    assert_eq!(m.read_u32(0x3000).unwrap(), 0x017f);
    assert_eq!(m.read_u64(0x3000).unwrap(), 0x017f);
    assert_eq!(m.read_f32(0x4000).unwrap(), 1.5);
    assert_eq!(m.read_i32(0x4004).unwrap(), -5);
}

#[test]
fn read_cstr_rules() {
    let mut mem = FakeMem::new();
    mem.add(0x5000, b"hello\0world".to_vec());
    mem.add(0x6000, vec![0xff, 0xfe, 0]);
    mem.add(0x7000, vec![b'a'; 8]);
    let m: &dyn MemRead = &mem;
    assert_eq!(m.read_cstr(0x5000, 16).unwrap(), "hello");
    assert_eq!(m.read_cstr(0x5000, 6).unwrap(), "hello");
    assert_eq!(m.read_cstr(0x5000, 5), Err(ReadError::Unterminated));
    assert_eq!(m.read_cstr(0x6000, 8), Err(ReadError::NotUtf8));
    // No NUL before the mapping ends: error, even though `max` would allow more.
    assert!(m.read_cstr(0x7000, 64).is_err());
}

#[test]
fn read_cstr_does_not_read_past_a_page_boundary_it_does_not_need() {
    let mut mem = FakeMem::new();
    // The string ends right before an unmapped page; reading `max` bytes in one go would fail.
    let mut b = vec![b'x'; 5];
    b.push(0);
    mem.add(0x8000 - 6, b);
    let m: &dyn MemRead = &mem;
    assert_eq!(m.read_cstr(0x8000 - 6, 512).unwrap(), "xxxxx");
}
