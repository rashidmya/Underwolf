use uw_spike_common::stats::{Comparison, FrameStats, Summary, compare, within_budget};

#[test]
fn summary_mean_fps_and_nearest_rank_p99() {
    let mut s = FrameStats::default();
    for _ in 0..98 {
        s.push_interval_ns(10_000_000)
    }
    for _ in 0..2 {
        s.push_interval_ns(20_000_000)
    }
    let sum = s.summary(0.0);
    assert!((sum.avg_fps - 98.039).abs() < 0.01, "avg_fps = {}", sum.avg_fps);
    assert_eq!(sum.p99_ms, 20.0);
    assert_eq!(sum.frames, 100);
}

#[test]
fn summary_skips_warmup_frames() {
    let mut s = FrameStats::default();
    for _ in 0..1000 {
        s.push_interval_ns(10_000_000)
    }
    assert_eq!(s.summary(5.0).frames, 500);
}

#[test]
fn compare_and_budget() {
    let c = compare(
        &Summary { avg_fps: 100.0, p99_ms: 10.0, frames: 1 },
        &Summary { avg_fps: 97.0, p99_ms: 10.3, frames: 1 },
    );
    assert!((c.fps_loss_pct - 3.0).abs() < 1e-9);
    assert!((c.p99_increase_pct - 3.0).abs() < 1e-9);
    assert!(within_budget(&c));
    assert!(!within_budget(&Comparison { fps_loss_pct: 3.01, p99_increase_pct: 0.0 }));
    assert!(!within_budget(&Comparison { fps_loss_pct: 0.0, p99_increase_pct: 3.01 }));
}

#[test]
fn from_csv_reads_intervals() {
    let dir = std::env::temp_dir().join(format!("uw-stats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("frames.csv");
    std::fs::write(&path, "100,10000000\n200,20000000\n").unwrap();
    let s = FrameStats::from_csv(&path).unwrap();
    let sum = s.summary(0.0);
    assert_eq!(sum.frames, 2);
    assert!((sum.avg_fps - 1000.0 / 15.0).abs() < 1e-9);
}
