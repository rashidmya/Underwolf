//! Linux-side injection: discover the game and its Wine session, and drive the Wine-side helper.
//! `discovery` is the ported spike launcher logic; `driver` turns the helper's JSON-lines events
//! into the commands the host sends back (load the overlay once the bootstrap connects).

pub mod driver;

#[cfg(unix)]
pub mod discovery;

/// A Unix path as Wine's `Z:` drive sees it (used for both the bootstrap and overlay paths).
pub fn to_wine_path(path: &std::path::Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// True when `name` (a bare exe name or a Windows/Unix path) names the exe `target`.
pub fn exe_matches(name: &str, target: &str) -> bool {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    base.eq_ignore_ascii_case(target)
}
