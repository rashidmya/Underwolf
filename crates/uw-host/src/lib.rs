//! The Linux host (design 2a). `discovery` finds the game and its Wine session (unix only);
//! `driver` holds the desired state and the reconcile commands; `supervisor` is the pure
//! supervision state machine (request ids, restarts, stop rules, the overlay timeout); `host`
//! runs it with real helper processes (unix only).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub mod driver;
pub mod supervisor;

#[cfg(unix)]
pub mod discovery;
#[cfg(unix)]
pub mod host;

/// A Unix path as Wine's `Z:` drive sees it (used for both the bootstrap and overlay paths).
pub fn to_wine_path(path: &std::path::Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!("Z:{}", abs.to_string_lossy().replace('/', "\\"))
}

/// True when `name` (a bare exe name or a Windows/Unix path) names the exe `target`.
pub fn exe_matches(name: &str, target: &str) -> bool {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    base.eq_ignore_ascii_case(target)
}

/// Request ids for commands to the helper, shared by the host's user and the host's own reconcile
/// commands so the two never collide. Clones share one counter; the first id is 1.
#[derive(Debug, Clone, Default)]
pub struct RequestIds(Arc<AtomicU32>);

impl RequestIds {
    pub fn next(&self) -> u32 {
        self.0.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
    }
}
