use crate::Result;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use uw_frame_proto::FrameError;
struct Inner {
    limit: u64,
    used: AtomicU64,
}
#[derive(Clone)]
pub struct Budget(Arc<Inner>);
pub struct BudgetReservation {
    budget: Arc<Inner>,
    bytes: u64,
}
impl Budget {
    pub fn new(limit: u64) -> Self {
        Self(Arc::new(Inner {
            limit,
            used: AtomicU64::new(0),
        }))
    }
    pub fn reserve(&self, bytes: u64) -> Result<BudgetReservation> {
        if bytes == 0 {
            return Err(FrameError::Invalid);
        }
        self.0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|&n| n <= self.0.limit)
            })
            .map_err(|_| FrameError::Capacity)?;
        Ok(BudgetReservation {
            budget: self.0.clone(),
            bytes,
        })
    }
    pub fn used(&self) -> u64 {
        self.0.used.load(Ordering::Acquire)
    }
}
impl Drop for BudgetReservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
