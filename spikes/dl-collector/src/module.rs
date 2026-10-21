use crate::mem::{FakeMem, MemRead, ReadError};

const MAX_SECTIONS: usize = 96;
const MAX_OPTIONAL_SIZE: usize = 1024;
const MAX_IMAGE_SIZE: u32 = 512 * 1024 * 1024;
const MAX_PE_OFFSET: usize = 0x1000;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub rva: u32,
    pub size: u32,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub base: u64,
    pub image_size: u32,
    pub sections: Vec<Section>,
}

struct RawSection {
    section: Section,
    raw_ptr: u32,
    raw_size: u32,
}

struct Parsed {
    image_base: u64,
    image_size: u32,
    size_of_headers: u32,
    sections: Vec<RawSection>,
}

fn bad(why: &'static str) -> ReadError {
    ReadError::BadImage(why)
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, ReadError> {
    let s = b
        .get(at..at.wrapping_add(2))
        .ok_or(bad("truncated headers"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ReadError> {
    let s = b
        .get(at..at.wrapping_add(4))
        .ok_or(bad("truncated headers"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, ReadError> {
    let s = b
        .get(at..at.wrapping_add(8))
        .ok_or(bad("truncated headers"))?;
    Ok(u64::from_le_bytes(s.try_into().unwrap()))
}

/// The PE offset and header sizes needed to know how many header bytes to read.
fn header_extent(b: &[u8]) -> Result<(usize, usize, usize), ReadError> {
    if u16_at(b, 0)? != 0x5a4d {
        return Err(bad("missing MZ signature"));
    }
    let pe = u32_at(b, 0x3c)? as usize;
    if !(0x40..=MAX_PE_OFFSET).contains(&pe) {
        return Err(bad("PE header offset out of range"));
    }
    if u32_at(b, pe)? != 0x4550 {
        return Err(bad("missing PE signature"));
    }
    let count = u16_at(b, pe + 6)? as usize;
    let optional_size = u16_at(b, pe + 20)? as usize;
    if !(1..=MAX_SECTIONS).contains(&count) {
        return Err(bad("section count out of range"));
    }
    if !(112..=MAX_OPTIONAL_SIZE).contains(&optional_size) {
        return Err(bad("optional header size out of range"));
    }
    Ok((pe, optional_size, count))
}

fn parse_headers(b: &[u8]) -> Result<Parsed, ReadError> {
    let (pe, optional_size, count) = header_extent(b)?;
    if u16_at(b, pe + 4)? != 0x8664 {
        return Err(bad("not an x86-64 image"));
    }
    let optional = pe + 24;
    if u16_at(b, optional)? != 0x20b {
        return Err(bad("not a PE32+ image"));
    }
    let image_base = u64_at(b, optional + 24)?;
    let image_size = u32_at(b, optional + 56)?;
    let size_of_headers = u32_at(b, optional + 60)?;
    if image_size == 0 || image_size > MAX_IMAGE_SIZE {
        return Err(bad("image size out of range"));
    }
    let mut sections = Vec::with_capacity(count);
    for i in 0..count {
        let at = optional + optional_size + i * 40;
        let virtual_size = u32_at(b, at + 8)?;
        let rva = u32_at(b, at + 12)?;
        let raw_size = u32_at(b, at + 16)?;
        let raw_ptr = u32_at(b, at + 20)?;
        let characteristics = u32_at(b, at + 36)?;
        let size = if virtual_size == 0 {
            raw_size
        } else {
            virtual_size
        };
        if rva as u64 + size as u64 > image_size as u64 {
            return Err(bad("section outside image"));
        }
        sections.push(RawSection {
            section: Section {
                rva,
                size,
                executable: characteristics & IMAGE_SCN_MEM_EXECUTE != 0,
            },
            raw_ptr,
            raw_size,
        });
    }
    Ok(Parsed {
        image_base,
        image_size,
        size_of_headers,
        sections,
    })
}

impl Module {
    /// Reads the PE headers of the image mapped at `base`.
    pub fn from_headers(m: &dyn MemRead, name: &str, base: u64) -> Result<Module, ReadError> {
        let mut dos = [0u8; 0x40];
        m.read(base, &mut dos)?;
        let pe = u32_at(&dos, 0x3c)? as usize;
        if !(0x40..=MAX_PE_OFFSET).contains(&pe) {
            return Err(bad("PE header offset out of range"));
        }
        let mut prefix = vec![0u8; pe + 24];
        m.read(base, &mut prefix)?;
        let (pe, optional_size, count) = header_extent(&prefix)?;
        let mut headers = vec![0u8; pe + 24 + optional_size + count * 40];
        m.read(base, &mut headers)?;
        let parsed = parse_headers(&headers)?;
        Ok(Module {
            name: name.to_string(),
            base,
            image_size: parsed.image_size,
            sections: parsed.sections.into_iter().map(|s| s.section).collect(),
        })
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr - self.base < self.image_size as u64
    }
}

/// Maps a PE file's headers and sections like the loader would, at the preferred ImageBase and
/// without relocation. Pointers stored in the file then dereference correctly. Returns the
/// memory and the preferred ImageBase.
pub fn file_image(bytes: &[u8]) -> Result<(FakeMem, u64), ReadError> {
    let parsed = parse_headers(bytes)?;
    let mut mem = FakeMem::new();
    let headers = (parsed.size_of_headers as usize).min(bytes.len());
    mem.add(parsed.image_base, bytes[..headers].to_vec());
    for s in &parsed.sections {
        let size = s.section.size as usize;
        if size == 0 {
            continue;
        }
        let ptr = s.raw_ptr as usize;
        let raw_end = ptr
            .checked_add(s.raw_size as usize)
            .ok_or(bad("raw data out of range"))?;
        if raw_end > bytes.len() {
            return Err(bad("section raw data truncated"));
        }
        let copy = (s.raw_size as usize).min(size);
        let mut data = vec![0u8; size];
        data[..copy].copy_from_slice(&bytes[ptr..ptr + copy]);
        mem.add(parsed.image_base + s.section.rva as u64, data);
    }
    Ok((mem, parsed.image_base))
}
