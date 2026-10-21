//! Info schema: categories of keys, each with a maximum value length.
//!
//! Serialized form (LE): `u32 category_count`, then per category `u16 name_len, name, u32 key_count`
//! and per key `u16 name_len, name, u32 max_len`. Keys are indexed across categories in order.

use crate::ChannelError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub categories: Vec<(String, Vec<(String, u32)>)>,
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ChannelError> {
        if self.0.len() < n {
            return Err(ChannelError::Truncated);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, ChannelError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, ChannelError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn name(&mut self) -> Result<String, ChannelError> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| ChannelError::Malformed)
    }
}

fn put_name(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&u16::try_from(s.len()).expect("schema name longer than 65535 bytes").to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Schema {
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.categories.len() as u32).to_le_bytes());
        for (cat, keys) in &self.categories {
            put_name(&mut out, cat);
            out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
            for (key, max) in keys {
                put_name(&mut out, key);
                out.extend_from_slice(&max.to_le_bytes());
            }
        }
        out
    }

    /// Parses a serialized schema. Trailing bytes are ignored.
    pub fn parse(b: &[u8]) -> Result<Schema, ChannelError> {
        let mut c = Cursor(b);
        let mut categories = Vec::new();
        for _ in 0..c.u32()? {
            let cat = c.name()?;
            let mut keys = Vec::new();
            for _ in 0..c.u32()? {
                let key = c.name()?;
                let max = c.u32()?;
                if max == u32::MAX {
                    return Err(ChannelError::Malformed);
                }
                keys.push((key, max));
            }
            categories.push((cat, keys));
        }
        Ok(Schema { categories })
    }

    pub fn key_index(&self, category: &str, key: &str) -> Option<usize> {
        let mut base = 0;
        for (cat, keys) in &self.categories {
            if cat == category {
                return keys.iter().position(|(k, _)| k == key).map(|i| base + i);
            }
            base += keys.len();
        }
        None
    }

    /// All keys in index order as `(category, key, max_len)`.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.categories
            .iter()
            .flat_map(|(cat, keys)| keys.iter().map(move |(k, max)| (cat.as_str(), k.as_str(), *max)))
    }
}
