//! The info schema (design 2b "Info store"): categories of keys, each key with a maximum value
//! length. Keys are numbered across categories in order. Serialized, little-endian, as a u32
//! category count, then per category `u16 len, name, u32 key_count`, and per key
//! `u16 len, name, u32 max_len`.
//!
//! The producer builds it once, at create (`Schema::new`); the helper parses the copy it finds in
//! the info store (`Schema::parse`). Both apply the design's limits: names of at most
//! `MAX_NAME_LEN` bytes, a `max_len` of at most `MAX_VALUE_LEN`, at most `MAX_KEYS` keys and at
//! most `MAX_CATEGORIES` categories. A name used twice (a category, or a key within its category)
//! does not fit either.

use serde::{Deserialize, Serialize};

pub const MAX_NAME_LEN: usize = 255;
pub const MAX_VALUE_LEN: u32 = 64 * 1024;
pub const MAX_KEYS: usize = 1024;
pub const MAX_CATEGORIES: usize = 1024;

/// One key and the most bytes its value may have. The same shape is in `data_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaKey {
    pub key: String,
    pub max_len: u32,
}

/// One category and its keys, in order. The same shape is in `data_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaCategory {
    pub category: String,
    pub keys: Vec<SchemaKey>,
}

impl SchemaCategory {
    /// `category` with these `(key, max_len)` pairs.
    pub fn new(category: &str, keys: &[(&str, u32)]) -> SchemaCategory {
        SchemaCategory {
            category: category.to_string(),
            keys: keys
                .iter()
                .map(|&(key, max_len)| SchemaKey {
                    key: key.to_string(),
                    max_len,
                })
                .collect(),
        }
    }
}

/// Why a schema does not fit the design's limits, or its bytes do not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    NameTooLong(usize),
    MaxLenTooLarge {
        key: String,
        max_len: u32,
    },
    TooManyKeys(usize),
    TooManyCategories(usize),
    /// A category, or a key within its category (`category.key`), named twice.
    Duplicate(String),
    /// Serialized bytes that end early, run on, or hold a name that is not UTF-8.
    Malformed(&'static str),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::NameTooLong(n) => write!(f, "a name of {n} bytes, over {MAX_NAME_LEN}"),
            SchemaError::MaxLenTooLarge { key, max_len } => {
                write!(f, "{key}: max_len {max_len}, over {MAX_VALUE_LEN}")
            }
            SchemaError::TooManyKeys(n) => write!(f, "{n} keys, over {MAX_KEYS}"),
            SchemaError::TooManyCategories(n) => {
                write!(f, "{n} categories, over {MAX_CATEGORIES}")
            }
            SchemaError::Duplicate(name) => write!(f, "{name} is named twice"),
            SchemaError::Malformed(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for SchemaError {}

/// A schema that fits the limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    categories: Vec<SchemaCategory>,
    key_count: usize,
}

fn check(categories: &[SchemaCategory]) -> Result<usize, SchemaError> {
    if categories.len() > MAX_CATEGORIES {
        return Err(SchemaError::TooManyCategories(categories.len()));
    }
    let key_count: usize = categories.iter().map(|c| c.keys.len()).sum();
    if key_count > MAX_KEYS {
        return Err(SchemaError::TooManyKeys(key_count));
    }
    for (i, c) in categories.iter().enumerate() {
        if c.category.len() > MAX_NAME_LEN {
            return Err(SchemaError::NameTooLong(c.category.len()));
        }
        if categories[..i].iter().any(|o| o.category == c.category) {
            return Err(SchemaError::Duplicate(c.category.clone()));
        }
        for (j, k) in c.keys.iter().enumerate() {
            if k.key.len() > MAX_NAME_LEN {
                return Err(SchemaError::NameTooLong(k.key.len()));
            }
            if k.max_len > MAX_VALUE_LEN {
                return Err(SchemaError::MaxLenTooLarge {
                    key: k.key.clone(),
                    max_len: k.max_len,
                });
            }
            if c.keys[..j].iter().any(|o| o.key == k.key) {
                return Err(SchemaError::Duplicate(format!("{}.{}", c.category, k.key)));
            }
        }
    }
    Ok(key_count)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SchemaError> {
        if self.0.len() < n {
            return Err(SchemaError::Malformed("the schema ends early"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, SchemaError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, SchemaError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn name(&mut self) -> Result<String, SchemaError> {
        let n = self.u16()? as usize;
        if n > MAX_NAME_LEN {
            return Err(SchemaError::NameTooLong(n));
        }
        String::from_utf8(self.take(n)?.to_vec())
            .map_err(|_| SchemaError::Malformed("a schema name is not UTF-8"))
    }
}

fn put_name(out: &mut Vec<u8>, s: &str) {
    // `check` keeps every name within MAX_NAME_LEN, so it fits a u16.
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Schema {
    /// The producer's schema, checked against the limits.
    pub fn new(categories: Vec<SchemaCategory>) -> Result<Schema, SchemaError> {
        let key_count = check(&categories)?;
        Ok(Schema {
            categories,
            key_count,
        })
    }

    pub fn categories(&self) -> &[SchemaCategory] {
        &self.categories
    }

    /// Keys in all categories.
    pub fn key_count(&self) -> usize {
        self.key_count
    }

    /// Every key in index order, as `(category, key, max_len)`.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.categories.iter().flat_map(|c| {
            c.keys
                .iter()
                .map(move |k| (c.category.as_str(), k.key.as_str(), k.max_len))
        })
    }

    /// The index of `category.key`, if the schema has it.
    pub fn key_index(&self, category: &str, key: &str) -> Option<usize> {
        let mut base = 0;
        for c in &self.categories {
            if c.category == category {
                return c.keys.iter().position(|k| k.key == key).map(|i| base + i);
            }
            base += c.keys.len();
        }
        None
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.categories.len() as u32).to_le_bytes());
        for c in &self.categories {
            put_name(&mut out, &c.category);
            out.extend_from_slice(&(c.keys.len() as u32).to_le_bytes());
            for k in &c.keys {
                put_name(&mut out, &k.key);
                out.extend_from_slice(&k.max_len.to_le_bytes());
            }
        }
        out
    }

    /// The helper's side: parse exactly `b` (bytes left over do not fit), with the same limits.
    pub fn parse(b: &[u8]) -> Result<Schema, SchemaError> {
        let mut c = Cursor(b);
        let mut categories = Vec::new();
        let mut keys_seen = 0usize;
        let category_count = c.u32()? as usize;
        if category_count > MAX_CATEGORIES {
            return Err(SchemaError::TooManyCategories(category_count));
        }
        for _ in 0..category_count {
            let category = c.name()?;
            let mut keys = Vec::new();
            for _ in 0..c.u32()? {
                keys_seen += 1;
                if keys_seen > MAX_KEYS {
                    return Err(SchemaError::TooManyKeys(keys_seen));
                }
                let key = c.name()?;
                let max_len = c.u32()?;
                keys.push(SchemaKey { key, max_len });
            }
            categories.push(SchemaCategory { category, keys });
        }
        if !c.0.is_empty() {
            return Err(SchemaError::Malformed("bytes left over after the schema"));
        }
        Schema::new(categories)
    }
}
