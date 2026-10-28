//! Upload fences release shared pixels. Separate present fences release binary
//! semaphore/presentation resources. Neither time nor receipt is completion.
use crate::resources::{Context, Texture};
use ash::vk;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uw_frame_channel::{CompleteResult, ReadLease};
use uw_frame_proto::{AllocationKey, FrameError};
pub type PollResult = (Vec<(AllocationKey, u64)>, Option<u64>);
pub struct Frame {
    pub ctx: Arc<Context>,
    pub view: vk::ImageView,
    pub framebuffer: vk::Framebuffer,
    pub cmd: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub present_fence: vk::Fence,
    pub semaphore: vk::Semaphore,
    pub submitted: bool,
    pub present_pending: bool,
    pub present_active: Arc<AtomicBool>,
    pub failed: bool,
    pub draw_reported: bool,
    pub epoch: u64,
    pub leases: Vec<ReadLease>,
    pub assets: Vec<Arc<Texture>>,
}
impl Frame {
    pub fn new(ctx: Arc<Context>) -> Self {
        Self {
            ctx,
            view: vk::ImageView::null(),
            framebuffer: vk::Framebuffer::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            present_fence: vk::Fence::null(),
            semaphore: vk::Semaphore::null(),
            submitted: false,
            present_pending: false,
            present_active: Arc::new(AtomicBool::new(false)),
            failed: false,
            draw_reported: false,
            epoch: 0,
            leases: vec![],
            assets: vec![],
        }
    }
    pub fn reusable(&self) -> bool {
        !self.failed
            && !self.submitted
            && !self.present_pending
            && !self.present_active.load(Ordering::Acquire)
            && self.leases.is_empty()
    }
    pub fn safe_to_retire(&self) -> bool {
        !self.submitted
            && !self.present_pending
            && !self.present_active.load(Ordering::Acquire)
            && self.leases.is_empty()
    }
    /// # Safety
    /// Called under render-state serialization for this live device; frame
    /// handles stay owned and destruction is externally ordered after worker stop.
    pub unsafe fn poll(&mut self) -> Result<PollResult, FrameError> {
        if self.failed && self.submitted {
            return Err(FrameError::Io);
        }
        let d = &self.ctx.life.device;
        let mut consumed = vec![];
        let mut progress = None;
        // SAFETY: these fences belong to this retained live frame. A signaled
        // upload fence proves all GPU reads; a present fence proves semaphore use.
        unsafe {
            if self.present_pending
                && !self.present_active.load(Ordering::Acquire)
                && d.get_fence_status(self.present_fence)
                    .map_err(|_| FrameError::Io)?
            {
                self.present_pending = false;
            }
            if self.submitted && d.get_fence_status(self.fence).map_err(|_| FrameError::Io)? {
                if !self.draw_reported {
                    self.draw_reported = true;
                    progress = Some(self.epoch);
                }
                let mut keep = vec![];
                let mut fault = None;
                for lease in std::mem::take(&mut self.leases) {
                    match lease.mapping().try_complete_after_gpu(&lease) {
                        Ok(CompleteResult::Completed) => {
                            consumed.push((lease.claim().key, lease.claim().seq))
                        }
                        Ok(CompleteResult::Busy) | Err(FrameError::Busy) => keep.push(lease),
                        Ok(CompleteResult::Stale) | Err(FrameError::OwnerDied) => {}
                        Err(e) => {
                            keep.push(lease);
                            fault = Some(e);
                        }
                    }
                }
                self.leases = keep;
                if let Some(e) = fault {
                    return Err(e);
                }
                if self.leases.is_empty() {
                    self.submitted = false;
                    self.assets.clear();
                }
            }
        }
        Ok((consumed, progress))
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        if !self.ctx.life.is_live() {
            return;
        }
        // SAFETY: frames are dropped only when safe_to_retire or during unsubmitted
        // constructor failure. Unresolved frames survive until device destruction.
        unsafe {
            let d = &self.ctx.life.device;
            d.destroy_framebuffer(self.framebuffer, None);
            d.destroy_image_view(self.view, None);
            d.destroy_fence(self.fence, None);
            d.destroy_fence(self.present_fence, None);
            d.destroy_semaphore(self.semaphore, None);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::DeviceLife;
    use ash::vk::Handle;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
    };
    use uw_frame_channel::{Budget, FrameMapping, Patch, PeerIdentity};
    use uw_frame_proto::*;
    static SERIAL: Mutex<()> = Mutex::new(());
    static UPLOAD: AtomicI32 = AtomicI32::new(1);
    static PRESENT: AtomicI32 = AtomicI32::new(1);
    unsafe extern "system" fn status(_: vk::Device, fence: vk::Fence) -> vk::Result {
        vk::Result::from_raw(if fence.as_raw() == 11 {
            UPLOAD.load(Ordering::SeqCst)
        } else {
            PRESENT.load(Ordering::SeqCst)
        })
    }
    fn frame() -> (Frame, Arc<FrameMapping>) {
        // SAFETY: the only called mock function has the exact fence-status ABI;
        // dead DeviceLife suppresses object destruction in this CPU-only fixture.
        let device = unsafe {
            ash::Device::load_with(
                |name| {
                    if name == c"vkGetFenceStatus" {
                        status as *const () as *const std::ffi::c_void
                    } else {
                        std::ptr::null()
                    }
                },
                vk::Device::null(),
            )
        };
        let ctx = Arc::new(Context {
            handles_live: AtomicBool::new(false),
            counts: crate::resources::ResourceCounts::default(),
            life: Arc::new(DeviceLife {
                device,
                live: AtomicBool::new(false),
            }),
            host: ash::ext::external_memory_host::DeviceFn::load(|_| std::ptr::null()),
            props: vk::PhysicalDeviceMemoryProperties::default(),
            alignment: 4096,
            layout: vk::DescriptorSetLayout::null(),
            sampler: vk::Sampler::null(),
            pool: vk::DescriptorPool::null(),
            budget: Budget::new(1),
        });
        let key = AllocationKey {
            session: SessionId([7; 16]),
            widget: WidgetKey {
                id: 1,
                generation: 1,
            },
            generation: 1,
        };
        let mapping = Arc::new(
            FrameMapping::create(
                key,
                FrameLayout::new(4, 1, 4096).unwrap(),
                PeerIdentity::current().unwrap(),
            )
            .unwrap(),
        );
        mapping
            .try_publish(
                1,
                Patch {
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 1,
                    },
                    stride: 16,
                    data: &[7; 16],
                },
            )
            .unwrap();
        let mut frame = Frame::new(ctx);
        frame.fence = vk::Fence::from_raw(11);
        frame.present_fence = vk::Fence::from_raw(22);
        frame.submitted = true;
        frame.present_pending = true;
        frame.epoch = 3;
        frame.leases.push(mapping.try_claim().unwrap().unwrap());
        (frame, mapping)
    }
    #[test]
    fn upload_completion_releases_pixels_but_does_not_release_presentation_resources() {
        let _serial = SERIAL.lock().unwrap();
        UPLOAD.store(0, Ordering::SeqCst);
        PRESENT.store(1, Ordering::SeqCst);
        let (mut frame, mapping) = frame();
        let replacement_key = AllocationKey {
            generation: 2,
            ..mapping.header().key
        };
        let replacement = FrameMapping::create(
            replacement_key,
            mapping.header().layout,
            PeerIdentity::current().unwrap(),
        )
        .unwrap();
        replacement
            .try_publish(
                1,
                Patch {
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 1,
                    },
                    stride: 16,
                    data: &[9; 16],
                },
            )
            .unwrap();
        // SAFETY: CPU-only mock submitted no real GPU work.
        let (done, progress) = unsafe { frame.poll() }.unwrap();
        assert_eq!(done, vec![(mapping.header().key, 1)]);
        assert_eq!(progress, Some(3));
        assert_eq!(mapping.try_state().unwrap().consumed, 1);
        let current = replacement.try_state().unwrap();
        assert_eq!(current.consumed, 0);
        assert_eq!(current.phase, Phase::Ready);
        assert!(!frame.safe_to_retire());
        assert!(!frame.reusable());
        PRESENT.store(0, Ordering::SeqCst);
        // SAFETY: mock presentation is now reported complete too.
        unsafe { frame.poll() }.unwrap();
        assert!(frame.safe_to_retire());
        assert!(frame.reusable());
    }
    #[test]
    fn unsignaled_or_lost_device_never_releases_or_consumes_pixels() {
        let _serial = SERIAL.lock().unwrap();
        UPLOAD.store(1, Ordering::SeqCst);
        PRESENT.store(1, Ordering::SeqCst);
        let (mut frame, mapping) = frame();
        // SAFETY: mock-only frame; polling performs no real GPU access.
        let initial = unsafe { frame.poll() }.unwrap();
        assert!(initial.0.is_empty());
        assert_eq!(mapping.try_state().unwrap().phase, Phase::Reading);
        UPLOAD.store(vk::Result::ERROR_DEVICE_LOST.as_raw(), Ordering::SeqCst);
        // SAFETY: error injection affects only the mock fence status.
        let result = unsafe { frame.poll() };
        assert!(result.is_err());
        assert_eq!(mapping.try_state().unwrap().consumed, 0);
        assert_eq!(frame.leases.len(), 1);
        assert!(!frame.safe_to_retire());
    }
    #[test]
    fn an_active_present_host_call_excludes_driver_fence_queries() {
        let _serial = SERIAL.lock().unwrap();
        UPLOAD.store(1, Ordering::SeqCst);
        PRESENT.store(vk::Result::ERROR_DEVICE_LOST.as_raw(), Ordering::SeqCst);
        let (mut frame, _mapping) = frame();
        frame.present_active.store(true, Ordering::Release);
        // SAFETY: CPU-only mock; its present-fence query deliberately errors, so
        // a successful poll proves no query occurs during the active host call.
        let during = unsafe { frame.poll() };
        assert!(during.is_ok());
        assert!(frame.present_pending);
        frame.present_active.store(false, Ordering::Release);
        // SAFETY: after the host call returns, ordinary fence error handling resumes.
        let after = unsafe { frame.poll() };
        assert!(after.is_err());
        assert!(frame.present_pending);
    }
}
