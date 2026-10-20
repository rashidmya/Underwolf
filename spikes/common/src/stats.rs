use std::io;
use std::path::Path;

/// Budget from the spec: overlay costs under ~2–3% FPS, no visible stutter.
pub const BUDGET_PCT: f64 = 3.0;
const EPSILON: f64 = 1e-9;

#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    intervals_ns: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub avg_fps: f64,
    pub p99_ms: f64,
    pub frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison {
    pub fps_loss_pct: f64,
    pub p99_increase_pct: f64,
}

impl FrameStats {
    pub fn push_interval_ns(&mut self, ns: u64) {
        self.intervals_ns.push(ns);
    }

    /// Reads the layer's CSV: one line per frame, `present_ns,interval_ns`.
    pub fn from_csv(path: impl AsRef<Path>) -> io::Result<FrameStats> {
        let text = std::fs::read_to_string(path)?;
        let mut stats = FrameStats::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let interval = line
                .split(',')
                .nth(1)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad CSV line {}: {line}", n + 1))
                })?;
            stats.push_interval_ns(interval);
        }
        Ok(stats)
    }

    /// Drops frames whose cumulative time is ≤ `skip_first_secs`, then reports
    /// mean FPS and nearest-rank p99 frame time (rank = ceil(0.99·n)).
    pub fn summary(&self, skip_first_secs: f64) -> Summary {
        let skip_ns = skip_first_secs * 1e9;
        let mut elapsed = 0.0;
        let mut kept: Vec<u64> = Vec::with_capacity(self.intervals_ns.len());
        for &ns in &self.intervals_ns {
            elapsed += ns as f64;
            if elapsed > skip_ns {
                kept.push(ns);
            }
        }
        if kept.is_empty() {
            return Summary { avg_fps: 0.0, p99_ms: 0.0, frames: 0 };
        }
        let total: f64 = kept.iter().map(|&ns| ns as f64).sum();
        let avg_fps = kept.len() as f64 * 1e9 / total;
        kept.sort_unstable();
        let rank = ((0.99 * kept.len() as f64).ceil() as usize).clamp(1, kept.len());
        let p99_ms = kept[rank - 1] as f64 / 1e6;
        Summary { avg_fps, p99_ms, frames: kept.len() }
    }
}

pub fn compare(base: &Summary, test: &Summary) -> Comparison {
    Comparison {
        fps_loss_pct: (base.avg_fps - test.avg_fps) / base.avg_fps * 100.0,
        p99_increase_pct: (test.p99_ms - base.p99_ms) / base.p99_ms * 100.0,
    }
}

pub fn within_budget(c: &Comparison) -> bool {
    c.fps_loss_pct <= BUDGET_PCT + EPSILON && c.p99_increase_pct <= BUDGET_PCT + EPSILON
}

/// Mean of several runs' summaries (frames are summed).
pub fn mean(runs: &[Summary]) -> Summary {
    let n = runs.len().max(1) as f64;
    Summary {
        avg_fps: runs.iter().map(|s| s.avg_fps).sum::<f64>() / n,
        p99_ms: runs.iter().map(|s| s.p99_ms).sum::<f64>() / n,
        frames: runs.iter().map(|s| s.frames).sum(),
    }
}
