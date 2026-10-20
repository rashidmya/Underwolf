use std::process::ExitCode;

use uw_spike_common::stats::{FrameStats, Summary, compare, mean, within_budget};

const SKIP_FIRST_SECS: f64 = 5.0;
const USAGE: &str = "usage: uw-spike-bench compare <base.csv>... -- <test.csv>...";

fn summarize(paths: &[String]) -> Result<Summary, String> {
    let runs = paths
        .iter()
        .map(|p| {
            FrameStats::from_csv(p)
                .map(|s| s.summary(SKIP_FIRST_SECS))
                .map_err(|e| format!("{p}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(mean(&runs))
}

fn run(args: &[String]) -> Result<bool, String> {
    let rest = match args.split_first() {
        Some((cmd, rest)) if cmd == "compare" => rest,
        _ => return Err(USAGE.into()),
    };
    let sep = rest.iter().position(|a| a == "--").ok_or(USAGE)?;
    let (base, test) = (&rest[..sep], &rest[sep + 1..]);
    if base.is_empty() || test.is_empty() {
        return Err(USAGE.into());
    }
    let (b, t) = (summarize(base)?, summarize(test)?);
    let c = compare(&b, &t);
    println!("base: {:>8.2} fps  p99 {:>7.3} ms  ({} frames, {} runs)", b.avg_fps, b.p99_ms, b.frames, base.len());
    println!("test: {:>8.2} fps  p99 {:>7.3} ms  ({} frames, {} runs)", t.avg_fps, t.p99_ms, t.frames, test.len());
    println!("fps loss {:+.2}%  p99 increase {:+.2}%", c.fps_loss_pct, c.p99_increase_pct);
    let pass = within_budget(&c);
    println!("{}", if pass { "PASS" } else { "FAIL" });
    Ok(pass)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
