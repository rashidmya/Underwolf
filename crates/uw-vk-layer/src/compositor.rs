//! The render mutex is the sole owner of mutable GPU/widget state. Present uses
//! try_lock; the channel/fence worker may lock normally and performs all I/O.
use crate::{
    DrawOutcome, RetireQueue,
    config::monotonic_ns,
    draw::{Quad, Renderer, Upload},
    resources::{Context, Texture},
};
use ash::vk::{self, Handle};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use uw_frame_channel::{Budget, CompleteResult, FrameMapping, PeerIdentity, ReadLease};
use uw_frame_proto::*;
pub struct Widget {
    pub mapping: Arc<FrameMapping>,
    pub texture: Option<Arc<Texture>>,
    pub pending: Option<ReadLease>,
    pub initialized: bool,
    pub layout: Option<(u64, i32, i32, u32, u32, i32, bool)>,
}
pub struct State {
    pub ctx: Arc<Context>,
    pub session: Option<SessionId>,
    pub available: bool,
    pub active: HashMap<u32, Widget>,
    retired: Vec<Widget>,
    pub swaps: HashMap<u64, Renderer>,
    pub old_swaps: RetireQueue<Renderer>,
    pub gpu_bytes: u64,
    pub next_swap: u64,
    pub queue: Option<u64>,
    epoch: u64,
    pub outgoing: VecDeque<Message>,
    pub close_connection: bool,
    shared_budget: Budget,
    last_allocation: u64,
    dim_revision: u64,
    dim: bool,
    last_progress_sent: u64,
    pending_progress: Option<Message>,
}
impl State {
    pub fn new(ctx: Arc<Context>) -> Self {
        Self {
            ctx,
            session: None,
            available: true,
            active: HashMap::new(),
            retired: vec![],
            swaps: HashMap::new(),
            old_swaps: RetireQueue::new(256),
            gpu_bytes: 0,
            next_swap: 0,
            queue: None,
            epoch: 0,
            outgoing: VecDeque::new(),
            close_connection: false,
            shared_budget: Budget::new(SHARED_BUDGET),
            last_allocation: 0,
            dim_revision: 0,
            dim: false,
            last_progress_sent: 0,
            pending_progress: None,
        }
    }
    pub fn notify(&mut self, m: Message) {
        if self.outgoing.len() >= 128 {
            self.available = false;
            self.close_connection = true;
            return;
        }
        self.outgoing.push_back(m);
    }
    pub fn fail(&mut self, reason: u32) {
        if self.available {
            self.notify(Message::Unavailable { key: None, reason });
        }
        self.available = false;
        self.close_connection = true;
    }
    /// # Safety
    /// Called after worker stop for a valid vkDestroyDevice call. The application
    /// has ordered all healthy-device queue work before teardown. Pins survive
    /// driver destruction even when a lost device cannot prove fence completion.
    pub unsafe fn teardown(&mut self) -> Vec<Arc<FrameMapping>> {
        let mut pins: Vec<_> = self
            .active
            .values()
            .chain(self.retired.iter())
            .map(|w| w.mapping.clone())
            .collect();
        for r in self.swaps.values().chain(self.old_swaps.iter()) {
            for f in &r.frames {
                pins.extend(f.leases.iter().map(|l| l.mapping().clone()));
                pins.extend(f.assets.iter().map(|t| t.mapping.clone()));
            }
        }
        self.active.clear();
        self.retired.clear();
        self.swaps.clear();
        drop(self.old_swaps.sweep(|_| true));
        // SAFETY: every layer-owned child was released under the caller's device
        // teardown contract; mapped payloads stay owned by the returned pins.
        unsafe {
            self.ctx.destroy_handles();
        }
        pins
    }
    pub fn metrics(&self) -> String {
        let (imports, images, descriptors) = self.ctx.counts.snapshot();
        let frames: usize = self
            .swaps
            .values()
            .chain(self.old_swaps.iter())
            .map(|r| r.frames.len())
            .sum();
        let pending: usize = self
            .swaps
            .values()
            .chain(self.old_swaps.iter())
            .flat_map(|r| r.frames.iter())
            .filter(|f| !f.safe_to_retire())
            .count();
        let uploads: usize = self
            .swaps
            .values()
            .chain(self.old_swaps.iter())
            .flat_map(|r| r.frames.iter())
            .map(|f| f.leases.len())
            .sum();
        format!(
            "{{\"imports\":{imports},\"images\":{images},\"descriptors\":{descriptors},\"gpuBytes\":\"{}\",\"gpuBudget\":{},\"sharedBudget\":{},\"mappings\":{},\"frames\":{frames},\"pendingFrames\":{pending},\"inFlightUploads\":{uploads},\"activeWidgets\":{},\"retiredWidgets\":{},\"available\":{}}}",
            self.gpu_bytes,
            self.ctx.budget.used(),
            self.shared_budget.used(),
            uw_frame_channel::live_mapping_count(),
            self.active.len(),
            self.retired.len(),
            self.available
        )
    }
    pub fn connected(&mut self, session: SessionId) {
        self.session = Some(session);
        self.last_allocation = 0;
        self.close_connection = false;
        self.dim_revision = 0;
        self.dim = false;
        let current =
            self.swaps
                .values()
                .max_by_key(|r| r.generation)
                .map(|r| Message::Swapchain {
                    generation: r.generation,
                    width: r.extent.width,
                    height: r.extent.height,
                    ready: true,
                });
        if let Some(m) = current {
            self.notify(m);
        }
    }
    pub fn disconnected(&mut self) {
        self.retired.extend(self.active.drain().map(|(_, w)| w));
        self.session = None;
        self.outgoing.clear();
        self.pending_progress = None;
        self.dim = false;
        self.close_connection = false;
    }
    pub fn incoming(
        &mut self,
        message: Message,
        fds: Vec<std::os::fd::OwnedFd>,
        peer: PeerIdentity,
    ) -> Result<()> {
        match message {
            Message::Register(header) => {
                if Some(header.key.session) != self.session
                    || header.key.generation <= self.last_allocation
                {
                    return Err(FrameError::Stale);
                }
                if self.active.len() + self.retired.len() >= 128
                    || (!self.active.contains_key(&header.key.widget.id) && self.active.len() >= 32)
                {
                    return Err(FrameError::Capacity);
                }
                // SAFETY: the authenticated native producer initializes this ABI;
                // open validates immutable metadata, seals, extents and identity.
                let mapping = Arc::new(unsafe {
                    FrameMapping::open_in(
                        &self.shared_budget,
                        &header.encode()?,
                        fds.try_into().map_err(|_| FrameError::Invalid)?,
                        peer,
                    )
                }?);
                if let Some(old) = self.active.remove(&header.key.widget.id) {
                    self.retired.push(old);
                }
                self.active.insert(
                    header.key.widget.id,
                    Widget {
                        mapping,
                        texture: None,
                        pending: None,
                        initialized: false,
                        layout: None,
                    },
                );
                self.last_allocation = header.key.generation;
                self.notify(Message::Registered { key: header.key });
            }
            Message::Dirty { key, .. } if fds.is_empty() && Some(key.session) == self.session => {} // shared metadata is authoritative
            Message::Layout {
                widget,
                revision,
                x,
                y,
                width,
                height,
                z,
                visible,
            } if fds.is_empty() => {
                if let Some(w) = self
                    .active
                    .get_mut(&widget.id)
                    .filter(|w| w.mapping.header().key.widget == widget)
                {
                    if width != w.mapping.header().layout.width
                        || height != w.mapping.header().layout.height
                    {
                        return Err(FrameError::Bounds);
                    }
                    if w.layout.is_none_or(|l| revision > l.0) {
                        w.layout = Some((revision, x, y, width, height, z, visible));
                    }
                }
            }
            Message::Remove { widget, revision } if fds.is_empty() => {
                if self.active.get(&widget.id).is_some_and(|w| {
                    w.mapping.header().key.widget == widget
                        && w.layout.is_none_or(|l| revision > l.0)
                }) {
                    self.retired.push(self.active.remove(&widget.id).unwrap());
                }
            }
            Message::Dimming { revision, active } if fds.is_empty() => {
                if revision > self.dim_revision {
                    self.dim_revision = revision;
                    self.dim = active;
                }
            }
            _ => return Err(FrameError::Invalid),
        }
        Ok(())
    }
    /// Polling runs on the worker, never waits on a fence or shared mutex.
    pub fn poll(&mut self) -> Result<()> {
        let mut notifications = vec![];
        let mut progress = None;
        for renderer in self.swaps.values_mut() {
            for frame in &mut renderer.frames {
                // SAFETY: worker holds render serialization, device remains live.
                let (done, draw) = unsafe { frame.poll() }?;
                notifications.extend(
                    done.into_iter()
                        .map(|(key, consumed)| Message::Consumed { key, consumed }),
                );
                if let Some(draw) = draw.filter(|_| renderer.generation == self.next_swap) {
                    progress = Some(Message::Progress {
                        swapchain_generation: renderer.generation,
                        completed_draw: draw,
                        monotonic_ns: monotonic_ns(),
                    });
                }
            }
        }
        // Retired renderers keep polling both fence classes. Dependencies are
        // generation identities, never reusable driver handles.
        let mut safe = std::collections::HashSet::new();
        let mut fault = None;
        for renderer in self.old_swaps.iter_mut() {
            for frame in &mut renderer.frames {
                // SAFETY: retained device and render-state serialization.
                match unsafe { frame.poll() } {
                    Ok((done, _)) => notifications.extend(
                        done.into_iter()
                            .map(|(key, consumed)| Message::Consumed { key, consumed }),
                    ),
                    Err(e) => fault = Some(e),
                }
            }
            if renderer.safe_to_retire() {
                safe.insert(renderer.generation);
            }
        }
        drop(
            self.old_swaps
                .sweep(|generation| safe.contains(&generation)),
        );
        if let Some(e) = fault {
            return Err(e);
        }
        for notification in notifications {
            if let Message::Consumed { key, .. } = notification
                && Some(key.session) == self.session
            {
                self.notify(notification);
            }
        }
        if let Some(progress) = progress {
            self.pending_progress = Some(progress);
        }
        let now = monotonic_ns();
        if now.saturating_sub(self.last_progress_sent) >= 100_000_000
            && let Some(m) = self.pending_progress.take()
            && self.session.is_some()
        {
            self.notify(m);
            self.last_progress_sent = now;
        }
        let mut keep = vec![];
        let mut retired_keys = vec![];
        let mut fault = None;
        for mut widget in std::mem::take(&mut self.retired) {
            if let Some(lease) = widget.pending.take() {
                match lease.mapping().try_cancel_unsubmitted(&lease) {
                    Ok(CompleteResult::Busy) | Err(FrameError::Busy) => {
                        widget.pending = Some(lease)
                    }
                    Ok(_) | Err(FrameError::OwnerDied) => {}
                    Err(e) => {
                        widget.pending = Some(lease);
                        fault = Some(e);
                    }
                }
            }
            let referenced = widget
                .texture
                .as_ref()
                .is_some_and(|t| Arc::strong_count(t) > 1);
            if widget.pending.is_none() && !referenced {
                let key = widget.mapping.header().key;
                drop(widget);
                if Some(key.session) == self.session {
                    retired_keys.push(key);
                }
            } else {
                keep.push(widget);
            }
        }
        self.retired = keep;
        for key in retired_keys {
            self.notify(Message::Retired { key });
        }
        if let Some(e) = fault {
            return Err(e);
        }
        if self.available && self.session.is_some() {
            for widget in self.active.values_mut() {
                if widget.pending.is_none() {
                    match widget.mapping.try_claim() {
                        Ok(Some(lease)) => {
                            widget.pending = Some(lease);
                            if widget.texture.is_none() {
                                // SAFETY: newly claimed immutable pixels; render mutex
                                // serializes all descriptor/import construction.
                                widget.texture = Some(
                                    unsafe {
                                        Texture::new(
                                            self.ctx.clone(),
                                            widget.pending.as_ref().unwrap(),
                                        )
                                    }
                                    .map_err(|_| FrameError::Unsupported)?,
                                );
                            }
                        }
                        Ok(None) | Err(FrameError::Busy) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(())
    }
    /// # Safety
    /// The app serializes this queue, index is acquired, and the caller attaches
    /// the returned signal plus this frame's present fence only on Submitted.
    pub unsafe fn draw(
        &mut self,
        swapchain: vk::SwapchainKHR,
        index: usize,
        queue: vk::Queue,
        family: u32,
        waits: &[vk::Semaphore],
    ) -> DrawOutcome {
        if !self.available || self.session.is_none() {
            return DrawOutcome::Unavailable;
        }
        if self.queue.is_some_and(|old| old != queue.as_raw()) {
            self.fail(2);
            return DrawOutcome::Unavailable;
        }
        let Some(renderer) = self.swaps.get_mut(&swapchain.as_raw()) else {
            return DrawOutcome::Unavailable;
        };
        if !renderer.frames.get(index).is_some_and(|f| f.reusable()) {
            return DrawOutcome::Skipped;
        }
        if self.active.is_empty() && !self.dim {
            return DrawOutcome::Skipped;
        }
        self.queue = Some(queue.as_raw());
        let mut quads = vec![];
        let mut uploads = vec![];
        let mut assets = vec![];
        if self.dim {
            quads.push(Quad {
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: renderer.extent.width,
                    height: renderer.extent.height,
                },
                x: 0,
                y: 0,
                texture: None,
                color: [0.0, 0.0, 0.0, 0.5],
            });
        }
        let mut widgets: Vec<_> = self.active.values().collect();
        widgets.sort_by_key(|w| {
            w.layout.map_or((0, w.mapping.header().key.widget.id), |l| {
                (l.5, w.mapping.header().key.widget.id)
            })
        });
        for widget in widgets {
            let Some(texture) = widget.texture.as_ref() else {
                continue;
            };
            if let Some(lease) = widget.pending.as_ref() {
                uploads.push(Upload {
                    texture,
                    damage: lease.claim().damage,
                    first: !widget.initialized,
                });
            }
            if (widget.initialized || widget.pending.is_some())
                && let Some((_, x, y, width, height, _, true)) = widget.layout
            {
                quads.push(Quad {
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    x,
                    y,
                    texture: Some(texture.clone()),
                    color: [0.0; 4],
                });
                assets.push(texture.clone());
            }
            if widget.pending.is_some() {
                assets.push(texture.clone());
            }
        }
        if quads.is_empty() && uploads.is_empty() {
            return DrawOutcome::Skipped;
        }
        let Some(epoch) = self.epoch.checked_add(1) else {
            self.fail(7);
            return DrawOutcome::Unavailable;
        };
        self.epoch = epoch;
        let upload_bytes: u64 = uploads
            .iter()
            .map(|u| u64::from(u.damage.width) * u64::from(u.damage.height) * 4)
            .sum();
        // SAFETY: caller's queue/index contract and retained assets; all mutable
        // state is serialized by this render lock, uploads have live leases.
        let outcome =
            unsafe { renderer.draw(queue, family, index, waits, &quads, &uploads, self.epoch) };
        if matches!(outcome, DrawOutcome::Submitted { .. }) {
            self.gpu_bytes += upload_bytes;
        }
        if renderer.frames[index].submitted {
            let frame = &mut renderer.frames[index];
            frame.assets = assets;
            for widget in self.active.values_mut() {
                if let Some(lease) = widget.pending.take() {
                    widget.initialized = matches!(outcome, DrawOutcome::Submitted { .. });
                    frame.leases.push(lease);
                }
            }
        }
        if outcome == DrawOutcome::Unavailable {
            self.fail(3);
        }
        outcome
    }
}
