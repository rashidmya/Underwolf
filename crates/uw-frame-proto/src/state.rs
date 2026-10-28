use crate::{AllocationKey, FrameError, FrameLayout, Rect, Result};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Ready,
    Reading,
    Invalid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub key: AllocationKey,
    pub seq: u64,
    pub damage: Rect,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSnapshot {
    pub key: AllocationKey,
    pub layout: FrameLayout,
    pub phase: Phase,
    pub produced: u64,
    pub consumed: u64,
    pub claimed: u64,
    pub damage: Option<Rect>,
    pub baseline: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameState {
    snapshot: StateSnapshot,
    reason: u32,
}
impl FrameState {
    pub fn new(key: AllocationKey, layout: FrameLayout) -> Self {
        let valid = key.validate().is_ok() && layout.validate().is_ok();
        Self {
            snapshot: StateSnapshot {
                key,
                layout,
                phase: if valid { Phase::Idle } else { Phase::Invalid },
                produced: 0,
                consumed: 0,
                claimed: 0,
                damage: None,
                baseline: false,
            },
            reason: if valid { 0 } else { 6 },
        }
    }
    pub fn publish(&mut self, seq: u64, damage: Rect) -> Result<()> {
        let s = &mut self.snapshot;
        if s.phase == Phase::Invalid {
            return Err(FrameError::Invalid);
        }
        if s.phase == Phase::Reading {
            return Err(FrameError::Busy);
        }
        if seq == 0 || seq <= s.produced {
            return Err(FrameError::Stale);
        }
        if Rect::checked(
            damage.x,
            damage.y,
            damage.width,
            damage.height,
            (s.layout.width, s.layout.height),
        )?
        .is_none()
        {
            return Err(FrameError::Invalid);
        }
        if s.produced == 0
            && damage
                != (Rect {
                    x: 0,
                    y: 0,
                    width: s.layout.width,
                    height: s.layout.height,
                })
        {
            return Err(FrameError::Invalid);
        }
        s.damage = Some(s.damage.map_or(damage, |old| old.union(damage)));
        s.produced = seq;
        s.phase = Phase::Ready;
        Ok(())
    }
    pub fn claim(&mut self) -> Result<Option<Claim>> {
        let s = &mut self.snapshot;
        match s.phase {
            Phase::Idle => Ok(None),
            Phase::Invalid => Err(FrameError::Invalid),
            Phase::Reading => Err(FrameError::Busy),
            Phase::Ready => {
                let damage = s.damage.ok_or(FrameError::Invalid)?;
                s.phase = Phase::Reading;
                s.claimed = s.produced;
                Ok(Some(Claim {
                    key: s.key,
                    seq: s.produced,
                    damage,
                }))
            }
        }
    }
    fn matches(&self, claim: Claim) -> bool {
        let s = &self.snapshot;
        s.phase == Phase::Reading
            && claim.key == s.key
            && claim.seq == s.claimed
            && s.damage == Some(claim.damage)
    }
    pub fn complete(&mut self, claim: Claim) -> Result<()> {
        if !self.matches(claim) {
            return Err(FrameError::Stale);
        }
        let s = &mut self.snapshot;
        s.phase = Phase::Idle;
        s.consumed = claim.seq;
        s.claimed = 0;
        s.damage = None;
        s.baseline = true;
        Ok(())
    }
    pub fn cancel_unsubmitted(&mut self, claim: Claim) -> Result<()> {
        if !self.matches(claim) {
            return Err(FrameError::Stale);
        }
        self.snapshot.phase = Phase::Ready;
        self.snapshot.claimed = 0;
        Ok(())
    }
    pub fn invalidate(&mut self) {
        self.snapshot.phase = Phase::Invalid;
        self.snapshot.claimed = 0;
        self.snapshot.damage = None;
        self.reason = 6;
    }
    pub fn snapshot(&self) -> StateSnapshot {
        self.snapshot
    }
    pub fn encode(&self) -> [u8; 64] {
        let s = &self.snapshot;
        let mut b = [0; 64];
        b[..8].copy_from_slice(&s.produced.to_le_bytes());
        b[8..16].copy_from_slice(&s.consumed.to_le_bytes());
        b[16..24].copy_from_slice(&s.claimed.to_le_bytes());
        if let Some(r) = s.damage {
            for (at, n) in [(24, r.x), (28, r.y), (32, r.width), (36, r.height)] {
                b[at..at + 4].copy_from_slice(&n.to_le_bytes());
            }
        }
        let phase: u32 = match s.phase {
            Phase::Idle => 0,
            Phase::Ready => 1,
            Phase::Reading => 2,
            Phase::Invalid => 3,
        };
        b[40..44].copy_from_slice(&phase.to_le_bytes());
        b[44..48].copy_from_slice(&u32::from(s.baseline).to_le_bytes());
        b[48..52].copy_from_slice(&self.reason.to_le_bytes());
        b
    }
    pub fn decode(key: AllocationKey, layout: FrameLayout, bytes: &[u8]) -> Result<Self> {
        use crate::layout::take;
        key.validate()?;
        layout.validate()?;
        if bytes.len() != 64 || bytes[52..].iter().any(|&n| n != 0) {
            return Err(FrameError::Invalid);
        }
        let u64at = |at| -> Result<u64> { Ok(u64::from_le_bytes(take(bytes, at)?)) };
        let u32at = |at| -> Result<u32> { Ok(u32::from_le_bytes(take(bytes, at)?)) };
        let produced = u64at(0)?;
        let consumed = u64at(8)?;
        let claimed = u64at(16)?;
        let baseline = match u32at(44)? {
            0 => false,
            1 => true,
            _ => return Err(FrameError::Invalid),
        };
        let reason = u32at(48)?;
        let r = Rect {
            x: u32at(24)?,
            y: u32at(28)?,
            width: u32at(32)?,
            height: u32at(36)?,
        };
        let damage = if r
            == (Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }) {
            None
        } else {
            Some(
                Rect::checked(r.x, r.y, r.width, r.height, (layout.width, layout.height))?
                    .ok_or(FrameError::Invalid)?,
            )
        };
        let phase = match u32at(40)? {
            0 => Phase::Idle,
            1 => Phase::Ready,
            2 => Phase::Reading,
            3 => Phase::Invalid,
            _ => return Err(FrameError::Invalid),
        };
        if consumed > produced || baseline != (consumed > 0) {
            return Err(FrameError::Invalid);
        }
        let full = Rect {
            x: 0,
            y: 0,
            width: layout.width,
            height: layout.height,
        };
        let valid = match phase {
            Phase::Idle => produced == consumed && claimed == 0 && damage.is_none() && reason == 0,
            Phase::Ready => {
                produced > consumed
                    && claimed == 0
                    && damage.is_some()
                    && (baseline || damage == Some(full))
                    && reason == 0
            }
            Phase::Reading => {
                produced > consumed
                    && claimed == produced
                    && damage.is_some()
                    && (baseline || damage == Some(full))
                    && reason == 0
            }
            Phase::Invalid => claimed == 0 && damage.is_none() && (1..=7).contains(&reason),
        };
        if !valid {
            return Err(FrameError::Invalid);
        }
        Ok(Self {
            snapshot: StateSnapshot {
                key,
                layout,
                phase,
                produced,
                consumed,
                claimed,
                damage,
                baseline,
            },
            reason,
        })
    }
}
