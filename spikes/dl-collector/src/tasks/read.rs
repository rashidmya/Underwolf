//! Read helpers shared by the tasks: vector headers, pointer array elements, byte ranges, the
//! check that a value read again is unchanged, and the game's team list.

use super::MAX_TEAMS;
use crate::mem::{MemRead, PTR_MAX, PTR_MIN, ReadError};
use crate::runner::TaskError;

/// Every team object in the game's team list at `root` (at most `MAX_TEAMS`, null slots
/// skipped), with what `read_team` reads from it. Every team is read again, then the list header
/// and its pointer array: values that changed meanwhile are not one state (`Torn`).
pub(super) fn read_team_list<T: PartialEq>(
    m: &dyn MemRead,
    root: u64,
    read_team: impl Fn(&dyn MemRead, u64) -> Result<T, TaskError>,
) -> Result<Vec<(u64, T)>, TaskError> {
    let list = VecHeader::read(m, root, MAX_TEAMS)?;
    let raw = list.elements(m, 8)?;
    let mut teams = Vec::new();
    for team_raw in raw.chunks_exact(8) {
        if let Some(team) = pointer(team_raw)? {
            teams.push((team, read_team(m, team)?));
        }
    }
    for (team, read) in &teams {
        ensure(read_team(m, *team)? == *read)?;
    }
    list.unchanged(m)?;
    ensure(list.count == 0 || read_bytes(m, list.data, raw.len())? == raw)?;
    Ok(teams)
}

/// A `{count: i32, data: pointer}` vector header, as read.
pub(super) struct VecHeader {
    addr: u64,
    raw: [u8; 16],
    pub(super) count: usize,
    pub(super) data: u64,
}

impl VecHeader {
    pub(super) fn read(m: &dyn MemRead, addr: u64, max: usize) -> Result<VecHeader, TaskError> {
        let mut raw = [0u8; 16];
        m.read(addr, &mut raw)?;
        let count = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let data = u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]);
        let count = usize::try_from(count)
            .ok()
            .filter(|&c| c <= max)
            .ok_or(ReadError::LimitExceeded)?;
        if count > 0 && !(PTR_MIN..=PTR_MAX).contains(&data) {
            return Err(ReadError::Implausible(data).into());
        }
        Ok(VecHeader {
            addr,
            raw,
            count,
            data,
        })
    }

    pub(super) fn unchanged(&self, m: &dyn MemRead) -> Result<(), TaskError> {
        let mut raw = [0u8; 16];
        m.read(self.addr, &mut raw)?;
        ensure(raw == self.raw)
    }

    pub(super) fn elements(&self, m: &dyn MemRead, size: usize) -> Result<Vec<u8>, TaskError> {
        read_bytes(m, self.data, self.count * size)
    }
}

pub(super) fn ensure(same: bool) -> Result<(), TaskError> {
    if same { Ok(()) } else { Err(TaskError::Torn) }
}

pub(super) fn read_bytes(m: &dyn MemRead, addr: u64, len: usize) -> Result<Vec<u8>, TaskError> {
    let mut buf = vec![0u8; len];
    if len > 0 {
        m.read(addr, &mut buf)?;
    }
    Ok(buf)
}

/// A pointer array element: null is `None`, an implausible value is an error.
pub(super) fn pointer(raw: &[u8]) -> Result<Option<u64>, TaskError> {
    let mut b = [0u8; 8];
    b.copy_from_slice(raw);
    match u64::from_le_bytes(b) {
        0 => Ok(None),
        p if (PTR_MIN..=PTR_MAX).contains(&p) => Ok(Some(p)),
        p => Err(ReadError::Implausible(p).into()),
    }
}
