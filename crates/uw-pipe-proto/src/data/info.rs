//! The info store (design 2b "Info store"): one slot per schema key, each with its own timestamp.
//!
//! The header is 32 bytes: magic `UWDI` @0, format version u32 @4, `schema_len` u32 @8,
//! `key_count` u32 @12, `slot_area_off` u32 @16, bytes 20 to 31 reserved. After it come:
//! 1. the serialized schema (`schema_len` bytes);
//! 2. one u64 timestamp per key, 8-aligned (0: never set);
//! 3. one slot per key: `len` u32 plus `max_len` bytes, padded to 8 (`len = u32::MAX`: null).
//!
//! The producer formats the store with `info_init` and writes keys with `info_set`, while it holds
//! the info lock. A key's timestamp only moves forward (`max(ts, prev + 1)`), and an unchanged
//! value keeps its slot and its timestamp. The helper reads with `InfoReader`, also under the
//! lock, and checks the header, every slot length and every value on every read.

use super::{FORMAT_VERSION, ReadError, Schema, rd_u32, rd_u64, wr_u32, wr_u64};

pub const INFO_MAGIC: [u8; 4] = *b"UWDI";
pub const INFO_HEADER_SIZE: usize = 32;

const OFF_VERSION: usize = 4;
const OFF_SCHEMA_LEN: usize = 8;
const OFF_KEY_COUNT: usize = 12;
const OFF_SLOT_AREA: usize = 16;
const NULL: u32 = u32::MAX;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Where everything sits in the info store of one schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoLayout {
    schema_len: usize,
    ts_off: usize,
    slot_area_off: usize,
    /// Per key: the slot's offset and the key's `max_len`.
    slots: Vec<(usize, u32)>,
    size: usize,
}

impl InfoLayout {
    /// The layout of `schema`. The schema's limits keep every offset far below `usize::MAX`
    /// (at most 1024 keys of 64 KiB).
    pub fn of(schema: &Schema) -> InfoLayout {
        let schema_len = schema.serialize().len();
        let ts_off = align8(INFO_HEADER_SIZE + schema_len);
        let slot_area_off = ts_off + 8 * schema.key_count();
        let mut off = slot_area_off;
        let mut slots = Vec::with_capacity(schema.key_count());
        for (_, _, max_len) in schema.keys() {
            slots.push((off, max_len));
            off += align8(4 + max_len as usize);
        }
        InfoLayout {
            schema_len,
            ts_off,
            slot_area_off,
            slots,
            size: off,
        }
    }

    /// The store's size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The `max_len` of key `index`.
    pub fn max_len(&self, index: usize) -> u32 {
        self.slots[index].1
    }

    fn ts_at(&self, index: usize) -> usize {
        self.ts_off + 8 * index
    }
}

/// The producer's side, at create: write the header and the schema, and leave every key never
/// set (timestamp 0) and null. Panics if `region` is smaller than `layout.size()`.
pub fn info_init(region: &mut [u8], schema: &Schema, layout: &InfoLayout) {
    let region = &mut region[..layout.size];
    region.fill(0);
    region[..4].copy_from_slice(&INFO_MAGIC);
    wr_u32(region, OFF_VERSION, FORMAT_VERSION);
    wr_u32(region, OFF_SCHEMA_LEN, layout.schema_len as u32);
    wr_u32(region, OFF_KEY_COUNT, layout.slots.len() as u32);
    wr_u32(region, OFF_SLOT_AREA, layout.slot_area_off as u32);
    region[INFO_HEADER_SIZE..INFO_HEADER_SIZE + layout.schema_len]
        .copy_from_slice(&schema.serialize());
    for &(off, _) in &layout.slots {
        wr_u32(region, off, NULL);
    }
}

/// The producer's side, under the info lock: set key `index` to `value` (`None`: null) at time
/// `ts`. Returns whether the key changed. A key that was never set changes even to null, so it
/// shows up in the helper's snapshot. Panics if `value` is longer than the key's `max_len`
/// (`Outbox::commit` checks that first).
pub fn info_set(
    region: &mut [u8],
    layout: &InfoLayout,
    index: usize,
    value: Option<&str>,
    ts: u64,
) -> bool {
    let (off, max_len) = layout.slots[index];
    let bytes = value.map(str::as_bytes);
    assert!(
        bytes.is_none_or(|v| v.len() <= max_len as usize),
        "info_set: a value over its max_len"
    );
    let ts_at = layout.ts_at(index);
    let prev = rd_u64(region, ts_at);
    let len = rd_u32(region, off);
    let same = prev != 0
        && match bytes {
            None => len == NULL,
            Some(v) => len as usize == v.len() && region[off + 4..off + 4 + v.len()] == *v,
        };
    if same {
        return false;
    }
    match bytes {
        None => wr_u32(region, off, NULL),
        Some(v) => {
            region[off + 4..off + 4 + v.len()].copy_from_slice(v);
            wr_u32(region, off, v.len() as u32);
        }
    }
    wr_u64(region, ts_at, ts.max(prev.saturating_add(1)));
    true
}

/// One key as the helper read it: the snapshot, or a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoValue {
    pub category: String,
    pub key: String,
    /// `None`: the key is null.
    pub value: Option<String>,
    pub ts: u64,
}

/// The helper's side: a checked view of one producer's info store.
pub struct InfoReader {
    schema: Schema,
    layout: InfoLayout,
    names: Vec<(String, String)>,
    /// The header and the schema as attached; every read compares them.
    head: Vec<u8>,
    /// Each key's timestamp at the last snapshot or changes.
    seen: Vec<u64>,
}

impl InfoReader {
    /// Check the store and read its schema (design flow step 2). Another format version is
    /// `Version`; a magic, size, offset or schema that does not fit is `Layout`.
    pub fn attach(region: &[u8]) -> Result<InfoReader, ReadError> {
        let layout_err = |why: String| Err(ReadError::Layout(why));
        if region.len() < INFO_HEADER_SIZE {
            return layout_err(format!(
                "info store is {} bytes, under its header",
                region.len()
            ));
        }
        if region[..4] != INFO_MAGIC {
            return layout_err("info store magic is not UWDI".into());
        }
        let version = rd_u32(region, OFF_VERSION);
        if version != FORMAT_VERSION {
            return Err(ReadError::Version(version));
        }
        let schema_len = rd_u32(region, OFF_SCHEMA_LEN) as usize;
        let Some(schema_bytes) = region[INFO_HEADER_SIZE..].get(..schema_len) else {
            return layout_err(format!(
                "info schema of {schema_len} bytes runs past the store"
            ));
        };
        let schema = match Schema::parse(schema_bytes) {
            Ok(s) => s,
            Err(e) => return layout_err(format!("info schema: {e}")),
        };
        let layout = InfoLayout::of(&schema);
        let key_count = rd_u32(region, OFF_KEY_COUNT) as usize;
        if key_count != schema.key_count() {
            return layout_err(format!(
                "info key_count {key_count}, but the schema has {} keys",
                schema.key_count()
            ));
        }
        let slot_area_off = rd_u32(region, OFF_SLOT_AREA) as usize;
        if slot_area_off != layout.slot_area_off {
            return layout_err(format!(
                "info slot_area_off {slot_area_off}, but the schema puts it at {}",
                layout.slot_area_off
            ));
        }
        if region.len() < layout.size {
            return layout_err(format!(
                "info store is {} bytes, its schema needs {}",
                region.len(),
                layout.size
            ));
        }
        let names = schema
            .keys()
            .map(|(c, k, _)| (c.to_string(), k.to_string()))
            .collect();
        let head = region[..INFO_HEADER_SIZE + schema_len].to_vec();
        Ok(InfoReader {
            seen: vec![0; key_count],
            schema,
            layout,
            names,
            head,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Every key that has been set (a non-zero timestamp), nulls included (design flow step 3).
    /// Later `changes` continue from exactly this snapshot.
    pub fn snapshot(&mut self, region: &[u8]) -> Result<Vec<InfoValue>, ReadError> {
        self.read_where(region, |ts, _| ts != 0)
    }

    /// The keys whose timestamps moved since the last snapshot or changes.
    pub fn changes(&mut self, region: &[u8]) -> Result<Vec<InfoValue>, ReadError> {
        self.read_where(region, |ts, seen| ts != seen)
    }

    fn read_where(
        &mut self,
        region: &[u8],
        wanted: impl Fn(u64, u64) -> bool,
    ) -> Result<Vec<InfoValue>, ReadError> {
        self.check(region)?;
        let mut out = Vec::new();
        let mut seen = self.seen.clone();
        for (i, last) in seen.iter_mut().enumerate() {
            let ts = rd_u64(region, self.layout.ts_at(i));
            if wanted(ts, *last) {
                out.push(self.read(region, i, ts)?);
            }
            *last = ts;
        }
        self.seen = seen;
        Ok(out)
    }

    /// The header and the schema are still what `attach` saw. If not, say why as `attach` would.
    fn check(&self, region: &[u8]) -> Result<(), ReadError> {
        if region.len() >= self.layout.size && region[..self.head.len()] == self.head[..] {
            return Ok(());
        }
        match InfoReader::attach(region) {
            Err(e) => Err(e),
            Ok(_) => Err(ReadError::Layout(
                "info header changed under the same producer".into(),
            )),
        }
    }

    fn read(&self, region: &[u8], i: usize, ts: u64) -> Result<InfoValue, ReadError> {
        let (off, max_len) = self.layout.slots[i];
        let (category, key) = &self.names[i];
        let len = rd_u32(region, off);
        let value = if len == NULL {
            None
        } else if len > max_len {
            return Err(ReadError::Layout(format!(
                "{category}.{key}: length {len} over its max_len {max_len}"
            )));
        } else {
            let bytes = region[off + 4..off + 4 + len as usize].to_vec();
            match String::from_utf8(bytes) {
                Ok(s) => Some(s),
                Err(_) => {
                    return Err(ReadError::Layout(format!(
                        "{category}.{key}: value is not UTF-8"
                    )));
                }
            }
        };
        Ok(InfoValue {
            category: category.clone(),
            key: key.clone(),
            value,
            ts,
        })
    }
}
