use std::fmt;

use crate::mem::{MemRead, ReadError};
use crate::module::Module;

/// Sections are scanned in chunks of this many bytes (plus pattern overlap).
pub const SCAN_CHUNK: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    BadToken(String),
    AllWildcards,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => f.write_str("empty pattern"),
            PatternError::BadToken(t) => write!(f, "bad pattern token {t:?}"),
            PatternError::AllWildcards => f.write_str("pattern has no fixed byte"),
        }
    }
}

impl std::error::Error for PatternError {}

/// A byte pattern with wildcards: `None` matches any byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(Vec<Option<u8>>);

impl Pattern {
    /// Parses space-separated tokens: two hex digits, or `??` for a wildcard.
    pub fn parse(s: &str) -> Result<Pattern, PatternError> {
        let mut bytes = Vec::new();
        for tok in s.split_whitespace() {
            if tok == "??" {
                bytes.push(None);
            } else if tok.len() == 2 && tok.bytes().all(|b| b.is_ascii_hexdigit()) {
                bytes.push(Some(u8::from_str_radix(tok, 16).unwrap_or(0)));
            } else {
                return Err(PatternError::BadToken(tok.to_string()));
            }
        }
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        if bytes.iter().all(Option::is_none) {
            return Err(PatternError::AllWildcards);
        }
        Ok(Pattern(bytes))
    }

    fn matches_at(&self, hay: &[u8]) -> bool {
        self.0
            .iter()
            .zip(hay)
            .all(|(p, b)| p.is_none_or(|p| p == *b))
    }
}

/// Addresses of every match inside the module's executable sections. More than `limit`
/// matches is `ReadError::LimitExceeded`, never a longer list.
pub fn scan(
    m: &dyn MemRead,
    module: &Module,
    p: &Pattern,
    limit: usize,
) -> Result<Vec<u64>, ReadError> {
    let plen = p.0.len();
    let anchor = p.0.iter().position(Option::is_some).unwrap_or(0);
    let anchor_byte = p.0[anchor].unwrap_or(0);
    let mut hits = Vec::new();
    let mut buf = Vec::new();
    for section in module.sections.iter().filter(|s| s.executable) {
        let start = module.base + section.rva as u64;
        let size = section.size as usize;
        let mut off = 0usize;
        while off + plen <= size {
            let len = (SCAN_CHUNK + plen - 1).min(size - off);
            buf.resize(len, 0);
            m.read(start + off as u64, &mut buf)?;
            // Start positions inside this chunk; the overlap tail belongs to the next one.
            let last = (len - plen).min(SCAN_CHUNK - 1);
            for i in 0..=last {
                if buf[i + anchor] == anchor_byte && p.matches_at(&buf[i..i + plen]) {
                    if hits.len() >= limit {
                        return Err(ReadError::LimitExceeded);
                    }
                    hits.push(start + (off + i) as u64);
                }
            }
            off += SCAN_CHUNK;
        }
    }
    Ok(hits)
}

/// Target of a rip-relative operand: the i32 displacement at `site + disp_at` is relative to
/// `site + next` (the address of the next instruction).
pub fn rip_target(m: &dyn MemRead, site: u64, disp_at: u32, next: u32) -> Result<u64, ReadError> {
    let disp = m.read_i32(site.wrapping_add(disp_at as u64))?;
    Ok(site
        .wrapping_add(next as u64)
        .wrapping_add(disp as i64 as u64))
}
