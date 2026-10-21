use std::collections::{BTreeMap, BTreeSet};

use crate::mem::{MemRead, ReadError};
use crate::module::Module;
use crate::pattern::rip_target;
use crate::roots::{CLASSES, MAX_SITES, RootError, find_sites};

pub const MAX_CLASSES: usize = 20_000;
pub const MAX_FIELDS: usize = 4096;
const NAME_MAX: usize = 512;
const FIELD_ENTRY: usize = 32;

struct Layout {
    name: &'static str,
    count_at: u64,
    table_at: u64,
}

/// Field-table layouts of a class record, in the order tried (as in underlock's source2.js).
const LAYOUTS: [Layout; 2] = [
    Layout {
        name: "disk-class-v2",
        count_at: 0x24,
        table_at: 0x30,
    },
    Layout {
        name: "gep-class-v1",
        count_at: 0x1c,
        table_at: 0x28,
    },
];

/// The class that identifies the right layout: it must be present and carry `PROBE_FIELD`.
const PROBE_CLASS: &str = "C_CitadelGameRules";
const PROBE_FIELD: &str = "m_eGameState";

/// Field offsets keyed by class and field name, read from the game's schema data.
#[derive(Debug, Default)]
pub struct SchemaFields {
    classes: BTreeMap<String, BTreeMap<String, u32>>,
    ambiguous: Vec<String>,
    unreadable: Vec<String>,
    layout: &'static str,
}

fn plausible(p: u64) -> Result<u64, ReadError> {
    if (crate::mem::PTR_MIN..=crate::mem::PTR_MAX).contains(&p) {
        Ok(p)
    } else {
        Err(ReadError::Implausible(p))
    }
}

/// Every `(class name, record address)` of one NUL-terminated class table.
fn read_class_table(
    m: &dyn MemRead,
    client: &Module,
    site: u64,
) -> Result<Vec<(String, u64)>, ReadError> {
    let table = rip_target(m, site, 3, 7)?;
    if !client.contains(table) {
        return Err(ReadError::Implausible(table));
    }
    let mut out = Vec::new();
    for i in 0..MAX_CLASSES {
        let record = m.read_u64(table + i as u64 * 8)?;
        if record == 0 {
            return Ok(out);
        }
        let record = plausible(record)?;
        let name = m.read_cstr(m.read_ptr(record + 8)?, NAME_MAX)?;
        out.push((name, record));
    }
    Err(ReadError::LimitExceeded)
}

fn read_fields(
    m: &dyn MemRead,
    record: u64,
    layout: &Layout,
) -> Result<Vec<(String, u32)>, ReadError> {
    let count = m.read_u16(record + layout.count_at)? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > MAX_FIELDS {
        return Err(ReadError::LimitExceeded);
    }
    let table = m.read_ptr(record + layout.table_at)?;
    let mut raw = vec![0u8; count * FIELD_ENTRY];
    m.read(table, &mut raw)?;
    let mut out = Vec::with_capacity(count);
    for e in raw.chunks_exact(FIELD_ENTRY) {
        let name_ptr = plausible(u64::from_le_bytes(e[..8].try_into().unwrap()))?;
        let offset = u32::from_le_bytes(e[16..20].try_into().unwrap());
        out.push((m.read_cstr(name_ptr, NAME_MAX)?, offset));
    }
    Ok(out)
}

impl SchemaFields {
    /// Resolves the class table by pattern and reads every class's fields. Classes whose name
    /// occurs in more than one record, and fields that occur twice in a class, are left out and
    /// listed in `ambiguous()`; classes that cannot be read are listed in `unreadable()`.
    pub fn load(m: &dyn MemRead, client: &Module) -> Result<SchemaFields, RootError> {
        let mut records: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        let mut tables = 0;
        for site in find_sites(m, client, CLASSES, MAX_SITES, "schema class table")? {
            // Not every matching instruction references a class table.
            if let Ok(list) = read_class_table(m, client, site) {
                tables += 1;
                for (name, record) in list {
                    records.entry(name).or_default().insert(record);
                }
            }
        }
        if tables == 0 {
            return Err(RootError::NotFound("schema class table"));
        }

        let layout = LAYOUTS
            .iter()
            .find(|l| {
                records.get(PROBE_CLASS).is_some_and(|set| {
                    set.iter().any(|&r| {
                        read_fields(m, r, l).is_ok_and(|f| f.iter().any(|(n, _)| n == PROBE_FIELD))
                    })
                })
            })
            .ok_or(RootError::NotFound("schema layout"))?;

        let mut out = SchemaFields {
            layout: layout.name,
            ..SchemaFields::default()
        };
        for (class, set) in &records {
            if set.len() > 1 {
                out.ambiguous
                    .push(format!("class {class} appears in {} records", set.len()));
                continue;
            }
            let Some(&record) = set.iter().next() else {
                continue;
            };
            let Ok(fields) = read_fields(m, record, layout) else {
                out.unreadable.push(class.clone());
                continue;
            };
            let mut map = BTreeMap::new();
            let mut dups = BTreeSet::new();
            for (name, offset) in fields {
                if map.insert(name.clone(), offset).is_some() {
                    dups.insert(name);
                }
            }
            for name in dups {
                map.remove(&name);
                out.ambiguous
                    .push(format!("field {class}.{name} appears more than once"));
            }
            out.classes.insert(class.clone(), map);
        }
        Ok(out)
    }

    pub fn get(&self, class: &str, field: &str) -> Option<u32> {
        self.classes.get(class)?.get(field).copied()
    }

    /// Why classes or fields were excluded as ambiguous.
    pub fn ambiguous(&self) -> &[String] {
        &self.ambiguous
    }

    /// Classes whose record or field table could not be read.
    pub fn unreadable(&self) -> &[String] {
        &self.unreadable
    }

    pub fn layout_name(&self) -> &'static str {
        self.layout
    }

    /// Every resolved class with its fields, sorted by name.
    pub fn classes(&self) -> &BTreeMap<String, BTreeMap<String, u32>> {
        &self.classes
    }
}
