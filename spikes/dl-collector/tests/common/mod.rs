#![allow(dead_code)]

use uw_spike_dlcollector::mem::FakeMem;
use uw_spike_dlcollector::module::{Module, Section};
use uw_spike_dlcollector::roots::CLASSES;

pub const BASE: u64 = 0x1_4000_0000;
pub const CODE_RVA: u32 = 0x1000;
pub const DATA_RVA: u32 = 0x10_0000;
pub const IMAGE_SIZE: u32 = 0x20_0000;

/// A synthetic client image: one executable code section and one data section.
pub struct Synth {
    pub code: Vec<u8>,
    pub data: Vec<u8>,
}

impl Synth {
    pub fn new() -> Synth {
        Synth {
            code: vec![0xcc; 0x2000],
            data: Vec::new(),
        }
    }

    pub fn code_va(&self, off: usize) -> u64 {
        BASE + CODE_RVA as u64 + off as u64
    }

    pub fn alloc(&mut self, bytes: &[u8]) -> u64 {
        while !self.data.len().is_multiple_of(8) {
            self.data.push(0);
        }
        let va = BASE + DATA_RVA as u64 + self.data.len() as u64;
        self.data.extend_from_slice(bytes);
        va
    }

    pub fn cstr(&mut self, s: &str) -> u64 {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        self.alloc(&b)
    }

    /// Writes `pat` at code offset `at` (wildcards become 0), then patches rip-relative
    /// displacements: `(disp_at, next, target)` makes `site + next + disp == target`.
    pub fn place(&mut self, at: usize, pat: &str, patches: &[(usize, u64, u64)]) {
        for (n, tok) in pat.split_whitespace().enumerate() {
            self.code[at + n] = if tok == "??" {
                0
            } else {
                u8::from_str_radix(tok, 16).unwrap()
            };
        }
        let site = self.code_va(at);
        for &(disp_at, next, target) in patches {
            let disp = (target as i64 - (site + next) as i64) as i32;
            self.code[at + disp_at..at + disp_at + 4].copy_from_slice(&disp.to_le_bytes());
        }
    }

    pub fn poke(&mut self, off: usize, byte: u8) {
        self.code[off] = byte;
    }

    /// A class record in the "disk-class-v2" layout (count u16 at +0x24, table at +0x30).
    pub fn class(&mut self, name: &str, fields: &[(&str, u32)]) -> u64 {
        let mut table = Vec::new();
        for (fname, off) in fields {
            let p = self.cstr(fname);
            let mut e = [0u8; 32];
            e[..8].copy_from_slice(&p.to_le_bytes());
            e[16..20].copy_from_slice(&off.to_le_bytes());
            table.extend_from_slice(&e);
        }
        let table_va = if fields.is_empty() {
            0
        } else {
            self.alloc(&table)
        };
        let name_va = self.cstr(name);
        let mut rec = [0u8; 0x40];
        rec[8..16].copy_from_slice(&name_va.to_le_bytes());
        rec[0x24..0x26].copy_from_slice(&(fields.len() as u16).to_le_bytes());
        rec[0x30..0x38].copy_from_slice(&table_va.to_le_bytes());
        self.alloc(&rec)
    }

    /// A NUL-terminated array of class record pointers; returns its address.
    pub fn class_table(&mut self, records: &[u64]) -> u64 {
        let mut b = Vec::new();
        for r in records {
            b.extend_from_slice(&r.to_le_bytes());
        }
        b.extend_from_slice(&0u64.to_le_bytes());
        self.alloc(&b)
    }

    /// Places the CLASSES pattern at code offset `at` referencing `table`.
    pub fn classes_site(&mut self, at: usize, table: u64) {
        self.place(at, CLASSES, &[(3, 7, table)]);
    }

    pub fn finish(&self) -> (FakeMem, Module) {
        let mut mem = FakeMem::new();
        mem.add(BASE + CODE_RVA as u64, self.code.clone());
        mem.add(BASE + DATA_RVA as u64, self.data.clone());
        let module = Module {
            name: "client.dll".into(),
            base: BASE,
            image_size: IMAGE_SIZE,
            sections: vec![
                Section {
                    rva: CODE_RVA,
                    size: self.code.len() as u32,
                    executable: true,
                },
                Section {
                    rva: DATA_RVA,
                    size: self.data.len() as u32,
                    executable: false,
                },
            ],
        };
        (mem, module)
    }
}
