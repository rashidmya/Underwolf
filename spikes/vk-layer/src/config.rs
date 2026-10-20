use std::sync::LazyLock;

use uw_spike_common::target::is_target_cmdline;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Passthrough,
    Quad,
    Composite,
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

pub static MODE: LazyLock<Mode> = LazyLock::new(|| match std::env::var("UW_SPIKE_MODE").as_deref() {
    Ok("quad") => Mode::Quad,
    Ok("composite") => Mode::Composite,
    _ => Mode::Passthrough,
});

/// The layer only does anything (stats, drawing, sockets) inside the target game,
/// or when `UW_SPIKE_FORCE_TARGET=1` (used to test with vkcube).
pub static IS_TARGET: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("UW_SPIKE_FORCE_TARGET").as_deref() == Ok("1") || argv0().is_some_and(|a| is_target_cmdline(&a))
});

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
