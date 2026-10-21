use std::fmt;

/// Pointers outside this range are never followed (user-mode canonical addresses).
pub const PTR_MIN: u64 = 0x1000;
pub const PTR_MAX: u64 = 0x7fff_ffff_ffff;

const CSTR_CHUNK: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Unmapped { addr: u64, len: usize },
    Implausible(u64),
    NotUtf8,
    Unterminated,
    BadImage(&'static str),
    LimitExceeded,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Unmapped { addr, len } => write!(f, "cannot read {len} bytes at {addr:#x}"),
            ReadError::Implausible(p) => write!(f, "implausible pointer {p:#x}"),
            ReadError::NotUtf8 => f.write_str("string is not valid UTF-8"),
            ReadError::Unterminated => f.write_str("string has no NUL within the limit"),
            ReadError::BadImage(why) => write!(f, "bad PE image: {why}"),
            ReadError::LimitExceeded => f.write_str("bound exceeded"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Reads target memory. An implementation must either fill all of `buf` or return an error;
/// it must never fault and never return a partial read as success.
pub trait MemRead {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ReadError>;
}

impl<'a> dyn MemRead + 'a {
    fn read_array<const N: usize>(&self, addr: u64) -> Result<[u8; N], ReadError> {
        let mut b = [0u8; N];
        self.read(addr, &mut b)?;
        Ok(b)
    }

    pub fn read_u8(&self, addr: u64) -> Result<u8, ReadError> {
        Ok(self.read_array::<1>(addr)?[0])
    }

    pub fn read_u16(&self, addr: u64) -> Result<u16, ReadError> {
        Ok(u16::from_le_bytes(self.read_array(addr)?))
    }

    pub fn read_u32(&self, addr: u64) -> Result<u32, ReadError> {
        Ok(u32::from_le_bytes(self.read_array(addr)?))
    }

    pub fn read_i32(&self, addr: u64) -> Result<i32, ReadError> {
        Ok(i32::from_le_bytes(self.read_array(addr)?))
    }

    pub fn read_u64(&self, addr: u64) -> Result<u64, ReadError> {
        Ok(u64::from_le_bytes(self.read_array(addr)?))
    }

    pub fn read_f32(&self, addr: u64) -> Result<f32, ReadError> {
        Ok(f32::from_le_bytes(self.read_array(addr)?))
    }

    /// A pointer value, accepted only inside `PTR_MIN..=PTR_MAX`.
    pub fn read_ptr(&self, addr: u64) -> Result<u64, ReadError> {
        let p = self.read_u64(addr)?;
        if (PTR_MIN..=PTR_MAX).contains(&p) {
            Ok(p)
        } else {
            Err(ReadError::Implausible(p))
        }
    }

    /// A UTF-8 string whose NUL terminator lies within the first `max` bytes at `addr`.
    /// Reads never cross a page boundary that the string does not need to cross.
    pub fn read_cstr(&self, addr: u64, max: usize) -> Result<String, ReadError> {
        let mut out = Vec::new();
        let mut at = addr;
        let mut buf = [0u8; CSTR_CHUNK];
        while out.len() < max {
            let to_page = 0x1000 - (at & 0xfff) as usize;
            let n = to_page.min(max - out.len()).min(CSTR_CHUNK);
            if self.read(at, &mut buf[..n]).is_err() {
                // The mapping may end inside this chunk (file images are not page-padded):
                // take the bytes that are readable one by one.
                let mut got = 0;
                while got < n && self.read(at + got as u64, &mut buf[got..got + 1]).is_ok() {
                    got += 1;
                    if buf[got - 1] == 0 {
                        break;
                    }
                }
                if got == 0 || (got < n && buf[got - 1] != 0) {
                    return Err(ReadError::Unmapped {
                        addr: at + got as u64,
                        len: 1,
                    });
                }
            }
            if let Some(end) = buf[..n].iter().position(|&b| b == 0) {
                out.extend_from_slice(&buf[..end]);
                return String::from_utf8(out).map_err(|_| ReadError::NotUtf8);
            }
            out.extend_from_slice(&buf[..n]);
            at = at.wrapping_add(n as u64);
        }
        Err(ReadError::Unterminated)
    }
}

/// Memory made of byte regions at fixed addresses (tests and offline file images).
#[derive(Default, Clone)]
pub struct FakeMem {
    regions: Vec<(u64, Vec<u8>)>,
}

impl FakeMem {
    pub fn new() -> FakeMem {
        FakeMem::default()
    }

    pub fn add(&mut self, base: u64, bytes: Vec<u8>) -> &mut FakeMem {
        self.regions.push((base, bytes));
        self
    }
}

impl MemRead for FakeMem {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let len = buf.len();
        let err = || ReadError::Unmapped { addr, len };
        let mut cur = addr;
        let mut done = 0;
        while done < buf.len() {
            let (base, data) = self
                .regions
                .iter()
                .find(|(b, d)| cur >= *b && cur - *b < d.len() as u64)
                .ok_or_else(err)?;
            let off = (cur - base) as usize;
            let n = (data.len() - off).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&data[off..off + n]);
            done += n;
            cur = cur.checked_add(n as u64).ok_or_else(err)?;
        }
        Ok(())
    }
}

/// The current process's own memory, read through `ReadProcessMemory` so that an unmapped
/// address is an error instead of an access violation.
#[cfg(windows)]
pub struct SelfProcess;

#[cfg(windows)]
impl MemRead for SelfProcess {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows::Win32::System::Threading::GetCurrentProcess;

        if buf.is_empty() {
            return Ok(());
        }
        let mut got = 0usize;
        // SAFETY: `buf` is a valid writable slice; the kernel validates the source address.
        let r = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                addr as usize as *const core::ffi::c_void,
                buf.as_mut_ptr().cast(),
                buf.len(),
                Some(&mut got),
            )
        };
        if r.is_ok() && got == buf.len() {
            Ok(())
        } else {
            Err(ReadError::Unmapped {
                addr,
                len: buf.len(),
            })
        }
    }
}
