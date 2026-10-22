//! The bootstrap's overlay-load decision, portable so it is unit-tested on Linux. The bootstrap
//! loads at most one overlay and never unloads it (design "Out of scope"), so a repeated request
//! for the same path is `already_loaded` and a different path is refused (`already_loaded_other`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Load,
    AlreadyLoaded,
    AlreadyLoadedOther,
}

#[derive(Default)]
pub struct OverlayState {
    loaded: Option<String>,
}

impl OverlayState {
    pub fn new() -> OverlayState {
        OverlayState::default()
    }

    pub fn decide(&self, requested: &str) -> Decision {
        match &self.loaded {
            None => Decision::Load,
            Some(p) if p.eq_ignore_ascii_case(requested) => Decision::AlreadyLoaded,
            Some(_) => Decision::AlreadyLoadedOther,
        }
    }

    pub fn mark_loaded(&mut self, path: String) {
        self.loaded = Some(path);
    }

    pub fn loaded_path(&self) -> Option<&str> {
        self.loaded.as_deref()
    }
}
