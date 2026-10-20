//! Redirects a loaded module's imported function pointers (its GOT slots) — how candidate C2
//! puts a filter in front of Wine's X11 event reads. Linux x86_64 ELF only.

use std::ffi::{CStr, c_int, c_void};

pub enum Module<'a> {
    /// The main executable.
    Main,
    /// A shared object whose path ends with this string (e.g. "/winex11.so").
    Suffix(&'a str),
}

const PT_DYNAMIC: u32 = 2;
const PT_GNU_RELRO: u32 = 0x6474_e552;
const DT_NULL: i64 = 0;
const DT_PLTRELSZ: i64 = 2;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_JMPREL: i64 = 23;
const R_X86_64_GLOB_DAT: u64 = 6;
const R_X86_64_JUMP_SLOT: u64 = 7;

#[repr(C)]
struct Dyn {
    tag: i64,
    val: u64,
}

#[repr(C)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

#[repr(C)]
struct Sym {
    name: u32,
    info: u8,
    other: u8,
    shndx: u16,
    value: u64,
    size: u64,
}

/// One imported-function pointer of a loaded module.
pub struct Slot {
    pub symbol: String,
    addr: *mut usize,
    /// The slot is in the module's RELRO range (read-only after relocation).
    relro: bool,
}

unsafe impl Send for Slot {}

impl Slot {
    pub fn get(&self) -> usize {
        unsafe { self.addr.read_volatile() }
    }

    /// Points the slot at `new`; returns the previous value.
    ///
    /// # Safety
    /// `new` must be a function with the imported symbol's exact signature.
    pub unsafe fn patch(&self, new: usize) -> usize {
        unsafe {
            let page = (self.addr as usize) & !4095;
            libc::mprotect(page as *mut c_void, 4096, libc::PROT_READ | libc::PROT_WRITE);
            let old = self.addr.read_volatile();
            self.addr.write_volatile(new);
            if self.relro {
                libc::mprotect(page as *mut c_void, 4096, libc::PROT_READ);
            }
            old
        }
    }
}

struct Search<'a> {
    module: &'a Module<'a>,
    symbols: &'a [&'a str],
    found: Vec<Slot>,
}

/// Finds `module`'s GOT slots (GLOB_DAT and JUMP_SLOT relocations) for `symbols`.
///
/// # Safety
/// Reads the dynamic-linker structures of the running process.
pub unsafe fn find_slots(module: Module<'_>, symbols: &[&str]) -> Vec<Slot> {
    let mut search = Search { module: &module, symbols, found: Vec::new() };
    unsafe { libc::dl_iterate_phdr(Some(visit), (&mut search as *mut Search<'_>).cast()) };
    search.found
}

unsafe extern "C" fn visit(info: *mut libc::dl_phdr_info, _size: usize, data: *mut c_void) -> c_int {
    unsafe {
        let search = &mut *(data as *mut Search<'_>);
        let info = &*info;
        let name = if info.dlpi_name.is_null() { "" } else { CStr::from_ptr(info.dlpi_name).to_str().unwrap_or("") };
        let matches = match search.module {
            // The main executable is reported first, with an empty name.
            Module::Main => name.is_empty() && search.found.is_empty(),
            Module::Suffix(suffix) => name.ends_with(suffix),
        };
        if !matches {
            return 0;
        }
        let base = info.dlpi_addr as usize;
        let phdrs = std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
        let mut relro = 0..0;
        let mut dynamic = None;
        for ph in phdrs {
            match ph.p_type {
                PT_DYNAMIC => dynamic = Some((base + ph.p_vaddr as usize) as *const Dyn),
                PT_GNU_RELRO => {
                    let start = base + ph.p_vaddr as usize;
                    relro = start..start + ph.p_memsz as usize;
                }
                _ => {}
            }
        }
        let Some(mut d) = dynamic else { return 1 };
        // The dynamic linker may already have rebased these addresses.
        let fix = |v: u64| if (v as usize) < base { base + v as usize } else { v as usize };
        let (mut strtab, mut symtab, mut rela, mut relasz, mut jmprel, mut pltrelsz) = (0, 0, 0, 0, 0, 0);
        while (*d).tag != DT_NULL {
            let v = (*d).val;
            match (*d).tag {
                DT_STRTAB => strtab = fix(v),
                DT_SYMTAB => symtab = fix(v),
                DT_RELA => rela = fix(v),
                DT_RELASZ => relasz = v as usize,
                DT_JMPREL => jmprel = fix(v),
                DT_PLTRELSZ => pltrelsz = v as usize,
                _ => {}
            }
            d = d.add(1);
        }
        if strtab == 0 || symtab == 0 {
            return 1;
        }
        for (table, size) in [(rela, relasz), (jmprel, pltrelsz)] {
            if table == 0 {
                continue;
            }
            let relocs = std::slice::from_raw_parts(table as *const Rela, size / size_of::<Rela>());
            for r in relocs {
                let ty = r.info & 0xffff_ffff;
                if ty != R_X86_64_GLOB_DAT && ty != R_X86_64_JUMP_SLOT {
                    continue;
                }
                let sym = &*((symtab as *const Sym).add((r.info >> 32) as usize));
                let sym_name = CStr::from_ptr((strtab + sym.name as usize) as *const libc::c_char);
                let Ok(sym_name) = sym_name.to_str() else { continue };
                if search.symbols.contains(&sym_name) {
                    let addr = base + r.offset as usize;
                    search.found.push(Slot { symbol: sym_name.to_owned(), addr: addr as *mut usize, relro: relro.contains(&addr) });
                }
            }
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn fake_getpid() -> libc::pid_t {
        4242
    }

    #[test]
    fn redirects_an_imported_function_and_restores_it() {
        let real = std::process::id();
        let slots = unsafe { find_slots(Module::Main, &["getpid"]) };
        assert!(!slots.is_empty(), "test binary imports getpid");
        let olds: Vec<usize> = slots.iter().map(|s| unsafe { s.patch(fake_getpid as *const () as usize) }).collect();
        let patched = std::process::id();
        for (s, old) in slots.iter().zip(olds) {
            unsafe { s.patch(old) };
        }
        assert_eq!(patched, 4242);
        assert_eq!(std::process::id(), real);
    }

    #[test]
    fn unknown_module_has_no_slots() {
        assert!(unsafe { find_slots(Module::Suffix("/no-such-module.so"), &["getpid"]) }.is_empty());
    }
}
