use uw_frame_proto::*;
#[derive(Debug, PartialEq, Eq)]
pub struct UploadRegion {
    pub offset: u64,
    pub row_length: u32,
    pub damage: Rect,
}
pub fn upload_region(layout: FrameLayout, damage: Rect) -> Result<UploadRegion> {
    layout.validate()?;
    let damage = Rect::checked(
        damage.x,
        damage.y,
        damage.width,
        damage.height,
        (layout.width, layout.height),
    )?
    .ok_or(FrameError::Invalid)?;
    Ok(UploadRegion {
        offset: u64::from(damage.y) * u64::from(layout.stride) + u64::from(damage.x) * 4,
        row_length: layout.stride / 4,
        damage,
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawOutcome {
    Submitted { epoch: u64 },
    Skipped,
    Unavailable,
}
pub fn submit_if_ready(
    capable: bool,
    ready: bool,
    epoch: u64,
    submit: impl FnOnce() -> bool,
) -> DrawOutcome {
    if !capable {
        DrawOutcome::Unavailable
    } else if !ready {
        DrawOutcome::Skipped
    } else if submit() {
        DrawOutcome::Submitted { epoch }
    } else {
        DrawOutcome::Unavailable
    }
}
pub fn target_process(path: &str, probe: bool) -> bool {
    let base = path.rsplit(['/', '\\']).next().unwrap_or("");
    base.eq_ignore_ascii_case("deadlock.exe")
        || base.eq_ignore_ascii_case("deadlock")
        || (base == "uw-render-probe" && probe)
}
pub struct RetireQueue<T> {
    items: Vec<(Vec<u64>, T)>,
    limit: usize,
}
impl<T> RetireQueue<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            items: vec![],
            limit,
        }
    }
    pub fn retire(&mut self, dependencies: Vec<u64>, item: T) -> std::result::Result<(), T> {
        if self.items.len() >= self.limit || dependencies.len() > 256 {
            return Err(item);
        }
        self.items.push((dependencies, item));
        Ok(())
    }
    pub fn sweep(&mut self, done: impl Fn(u64) -> bool) -> Vec<T> {
        let mut ready = vec![];
        let mut keep = vec![];
        for (dependencies, item) in self.items.drain(..) {
            if dependencies.iter().copied().all(&done) {
                ready.push(item);
            } else {
                keep.push((dependencies, item));
            }
        }
        self.items = keep;
        ready
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items.iter_mut().map(|(_, item)| item)
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter().map(|(_, item)| item)
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
