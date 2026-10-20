use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

use uw_spike_common::target::is_target_cmdline;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    Passthrough = 0,
    Quad = 1,
    Composite = 2,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Passthrough => "passthrough",
            Mode::Quad => "quad",
            Mode::Composite => "composite",
        }
    }
}

pub fn parse_mode(s: &str) -> Option<Mode> {
    match s.trim() {
        "passthrough" => Some(Mode::Passthrough),
        "quad" => Some(Mode::Quad),
        "composite" => Some(Mode::Composite),
        _ => None,
    }
}

pub fn csv_name(pid: u32, mode: Mode, segment: u32) -> String {
    format!("frames-{pid}-{}-{segment}.csv", mode.as_str())
}

/// Current mode. Starts from `UW_SPIKE_MODE`; the stats thread updates it from the
/// control file (`~/.cache/underwolf-spike/control`) so benchmarks can switch modes
/// without restarting the game.
static MODE: AtomicU8 = AtomicU8::new(u8::MAX);

fn encode(m: Mode) -> u8 {
    m as u8
}

fn decode(v: u8) -> Mode {
    match v {
        1 => Mode::Quad,
        2 => Mode::Composite,
        _ => Mode::Passthrough,
    }
}

pub fn current_mode() -> Mode {
    let v = MODE.load(Ordering::Relaxed);
    if v != u8::MAX {
        return decode(v);
    }
    let initial = std::env::var("UW_SPIKE_MODE").ok().and_then(|s| parse_mode(&s)).unwrap_or(Mode::Passthrough);
    let _ = MODE.compare_exchange(u8::MAX, encode(initial), Ordering::Relaxed, Ordering::Relaxed);
    decode(MODE.load(Ordering::Relaxed))
}

pub fn set_mode(m: Mode) {
    MODE.store(encode(m), Ordering::Relaxed);
}

/// The layer only does anything (stats, drawing, sockets) inside the target game,
/// or when `UW_SPIKE_FORCE_TARGET=1` (used to test with vkcube).
pub static IS_TARGET: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("UW_SPIKE_FORCE_TARGET").as_deref() == Ok("1") || argv0().is_some_and(|a| is_target_cmdline(&a))
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_control_file_contents() {
        assert_eq!(parse_mode("quad\n"), Some(Mode::Quad));
        assert_eq!(parse_mode(" passthrough "), Some(Mode::Passthrough));
        assert_eq!(parse_mode("composite"), Some(Mode::Composite));
        assert_eq!(parse_mode("bogus"), None);
        assert_eq!(parse_mode(""), None);
    }

    #[test]
    fn segment_csv_names() {
        assert_eq!(csv_name(42, Mode::Quad, 3), "frames-42-quad-3.csv");
    }

    #[test]
    fn mode_can_change_at_runtime() {
        set_mode(Mode::Quad);
        assert_eq!(current_mode(), Mode::Quad);
        set_mode(Mode::Passthrough);
        assert_eq!(current_mode(), Mode::Passthrough);
    }
}

fn argv0() -> Option<String> {
    #[cfg(unix)]
    {
        let raw = std::fs::read("/proc/self/cmdline").ok()?;
        let first = raw.split(|&b| b == 0).next()?;
        Some(String::from_utf8_lossy(first).into_owned())
    }
    #[cfg(not(unix))]
    {
        std::env::args().next()
    }
}
