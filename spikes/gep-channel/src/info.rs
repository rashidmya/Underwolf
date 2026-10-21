//! Info store: one slot per schema key, with a per-key timestamp that readers diff against.
//!
//! Header (32 bytes) `{magic "UWGI", version u32, schema_len u32, key_count u32, slot_area_off u32,
//! pad u32, commit_seq u64}`, then the serialized schema, then `key_count × u64` timestamps (8-aligned),
//! then the slots `{len u32, bytes[max]}` each padded to 8. `len == u32::MAX` means null.
//! A writer always moves a key's timestamp forward when its value changes, so a reader that sees a
//! different timestamp has seen a change. Timestamp 0 means "never set".

use crate::{ChannelError, FORMAT_VERSION, Schema, rd_u32, rd_u64, wr_u32, wr_u64};

const MAGIC: &[u8; 4] = b"UWGI";
const HEADER: usize = 32;
const OFF_SCHEMA_LEN: usize = 8;
const OFF_KEY_COUNT: usize = 12;
const OFF_SLOT_AREA: usize = 16;
const OFF_COMMIT_SEQ: usize = 24;
const NULL: u32 = u32::MAX;

fn align8(n: usize) -> Option<usize> {
    n.checked_add(7).map(|v| v & !7)
}

struct Layout {
    ts_off: usize,
    slot_area_off: usize,
    slot_offs: Vec<usize>,
    maxes: Vec<u32>,
    total: usize,
}

impl Layout {
    fn of(schema: &Schema, schema_len: usize) -> Result<Layout, ChannelError> {
        let ts_off = align8(HEADER + schema_len).ok_or(ChannelError::Truncated)?;
        let key_count = schema.keys().count();
        let slot_area_off = key_count.checked_mul(8).and_then(|n| n.checked_add(ts_off)).ok_or(ChannelError::Truncated)?;
        let mut off = slot_area_off;
        let mut slot_offs = Vec::with_capacity(key_count);
        let mut maxes = Vec::with_capacity(key_count);
        for (_, _, max) in schema.keys() {
            slot_offs.push(off);
            maxes.push(max);
            off = align8(4 + max as usize).and_then(|s| off.checked_add(s)).ok_or(ChannelError::Truncated)?;
        }
        Ok(Layout { ts_off, slot_area_off, slot_offs, maxes, total: off })
    }

    fn ts_at(&self, i: usize) -> usize {
        self.ts_off + i * 8
    }
}

pub fn info_region_size(schema: &Schema) -> usize {
    let len = schema.serialize().len();
    Layout::of(schema, len).map(|l| l.total).unwrap_or(usize::MAX)
}

fn read_layout(region: &[u8]) -> Result<(Schema, Layout), ChannelError> {
    if region.len() < HEADER {
        return Err(ChannelError::Truncated);
    }
    if &region[..4] != MAGIC {
        return Err(ChannelError::BadMagic);
    }
    if rd_u32(region, 4) != FORMAT_VERSION {
        return Err(ChannelError::BadVersion);
    }
    let schema_len = rd_u32(region, OFF_SCHEMA_LEN) as usize;
    let body = region.get(HEADER..HEADER.saturating_add(schema_len)).ok_or(ChannelError::Truncated)?;
    let schema = Schema::parse(body)?;
    let layout = Layout::of(&schema, schema_len)?;
    if rd_u32(region, OFF_KEY_COUNT) as usize != layout.maxes.len() || rd_u32(region, OFF_SLOT_AREA) as usize != layout.slot_area_off {
        return Err(ChannelError::Malformed);
    }
    if region.len() < layout.total {
        return Err(ChannelError::Truncated);
    }
    Ok((schema, layout))
}

pub struct InfoWriter<'a> {
    region: &'a mut [u8],
    schema: Schema,
    layout: Layout,
}

impl<'a> InfoWriter<'a> {
    /// Formats `region` for `schema`: every key starts null with timestamp 0.
    pub fn init(region: &'a mut [u8], schema: &Schema) -> Result<InfoWriter<'a>, ChannelError> {
        let schema_bytes = schema.serialize();
        let layout = Layout::of(schema, schema_bytes.len())?;
        if region.len() < layout.total {
            return Err(ChannelError::Truncated);
        }
        let region = &mut region[..layout.total];
        region.fill(0);
        region[..4].copy_from_slice(MAGIC);
        wr_u32(region, 4, FORMAT_VERSION);
        wr_u32(region, OFF_SCHEMA_LEN, schema_bytes.len() as u32);
        wr_u32(region, OFF_KEY_COUNT, layout.maxes.len() as u32);
        wr_u32(region, OFF_SLOT_AREA, layout.slot_area_off as u32);
        region[HEADER..HEADER + schema_bytes.len()].copy_from_slice(&schema_bytes);
        for &off in &layout.slot_offs {
            wr_u32(region, off, NULL);
        }
        Ok(InfoWriter { region, schema: schema.clone(), layout })
    }

    /// Takes over a region that `init` already formatted, keeping its values.
    pub fn attach(region: &'a mut [u8]) -> Result<InfoWriter<'a>, ChannelError> {
        let (schema, layout) = read_layout(region)?;
        let total = layout.total;
        Ok(InfoWriter { region: &mut region[..total], schema, layout })
    }

    pub fn begin(&mut self) -> Txn<'_, 'a> {
        Txn { w: self, changed: false }
    }
}

pub struct Txn<'w, 'a> {
    w: &'w mut InfoWriter<'a>,
    changed: bool,
}

impl Txn<'_, '_> {
    /// Sets a key (`None` clears it to null). An unchanged value leaves the slot and timestamp alone.
    pub fn set(&mut self, category: &str, key: &str, value: Option<&[u8]>, ts: u64) -> Result<(), ChannelError> {
        let w = &mut *self.w;
        let i = w.schema.key_index(category, key).ok_or(ChannelError::UnknownKey)?;
        if value.is_some_and(|v| v.len() > w.layout.maxes[i] as usize) {
            return Err(ChannelError::TooLong);
        }
        let off = w.layout.slot_offs[i];
        let cur_len = rd_u32(w.region, off);
        let same = match value {
            None => cur_len == NULL,
            Some(v) => cur_len as usize == v.len() && cur_len != NULL && &w.region[off + 4..off + 4 + v.len()] == v,
        };
        if same {
            return Ok(());
        }
        match value {
            None => wr_u32(w.region, off, NULL),
            Some(v) => {
                w.region[off + 4..off + 4 + v.len()].copy_from_slice(v);
                wr_u32(w.region, off, v.len() as u32);
            }
        }
        let prev = rd_u64(w.region, w.layout.ts_at(i));
        wr_u64(w.region, w.layout.ts_at(i), ts.max(prev.saturating_add(1)));
        self.changed = true;
        Ok(())
    }

    /// Returns true if any value changed (the caller then signals the consumer once).
    pub fn commit(self) -> bool {
        if self.changed {
            let w = self.w;
            let seq = rd_u64(w.region, OFF_COMMIT_SEQ);
            wr_u64(w.region, OFF_COMMIT_SEQ, seq.wrapping_add(1));
        }
        self.changed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoChange {
    pub category: String,
    pub key: String,
    pub value: Option<Vec<u8>>,
    pub ts: u64,
}

pub struct InfoReader {
    schema: Schema,
    layout: Layout,
    last_ts: Vec<u64>,
}

impl InfoReader {
    /// Validates the region and records the current timestamps, so `changed` reports only later changes.
    pub fn attach(region: &[u8]) -> Result<InfoReader, ChannelError> {
        let (schema, layout) = read_layout(region)?;
        let last_ts = (0..layout.maxes.len()).map(|i| rd_u64(region, layout.ts_at(i))).collect();
        Ok(InfoReader { schema, layout, last_ts })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Bytes of the region this reader covers.
    pub fn region_size(&self) -> usize {
        self.layout.total
    }

    fn read(&self, region: &[u8], i: usize) -> Option<InfoChange> {
        let (category, key, _) = self.schema.keys().nth(i)?;
        let off = self.layout.slot_offs[i];
        let len = rd_u32(region, off);
        let value = if len == NULL {
            None
        } else {
            let len = (len as usize).min(self.layout.maxes[i] as usize);
            Some(region[off + 4..off + 4 + len].to_vec())
        };
        Some(InfoChange { category: category.into(), key: key.into(), value, ts: rd_u64(region, self.layout.ts_at(i)) })
    }

    /// Keys whose timestamp differs from the last one this reader saw.
    pub fn changed(&mut self, region: &[u8]) -> Vec<InfoChange> {
        if region.len() < self.layout.total {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0..self.last_ts.len() {
            let ts = rd_u64(region, self.layout.ts_at(i));
            if ts != self.last_ts[i] {
                self.last_ts[i] = ts;
                out.extend(self.read(region, i));
            }
        }
        out
    }

    /// Every key that has ever been set, regardless of what this reader saw before.
    pub fn snapshot(&self, region: &[u8]) -> Vec<InfoChange> {
        if region.len() < self.layout.total {
            return Vec::new();
        }
        (0..self.last_ts.len())
            .filter(|&i| rd_u64(region, self.layout.ts_at(i)) != 0)
            .filter_map(|i| self.read(region, i))
            .collect()
    }
}
