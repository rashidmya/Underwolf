#[cfg(target_os = "linux")]
#[path = "../tests/support/gpu.rs"]
mod gpu;
#[cfg(target_os = "linux")]
#[path = "../tests/support/x11.rs"]
mod x11;
#[cfg(target_os = "linux")]
pub(crate) use gpu::{lifetime, loader_abi, resources};
#[cfg(target_os = "linux")]
pub use uw_vk_layer::{DrawOutcome, submit_if_ready, upload_region};
#[cfg(target_os = "linux")]
mod wsi {
    use super::*;
    use ash::vk;
    use std::{
        io::{self, BufRead},
        sync::mpsc,
        time::{Duration, Instant},
    };
    fn err(e: impl std::fmt::Debug) -> String {
        format!("{e:?}")
    }
    struct Slot {
        cmd: vk::CommandBuffer,
        fence: vk::Fence,
        acquire: vk::Semaphore,
    }
    struct Swap {
        device: ash::Device,
        life: std::sync::Arc<resources::DeviceLife>,
        fns: ash::khr::swapchain::Device,
        swap: vk::SwapchainKHR,
        images: Vec<vk::Image>,
        initialized: Vec<bool>,
        slots: Vec<Slot>,
        pool: vk::CommandPool,
        next: usize,
    }
    impl Swap {
        fn new(
            gpu: &gpu::Gpu,
            surface: vk::SurfaceKHR,
            window: &x11::Window,
            old: vk::SwapchainKHR,
        ) -> Result<Self, String> {
            // SAFETY: owned live Xlib surface and device; all local arrays retained.
            unsafe {
                let surface_fns = ash::khr::surface::Instance::new(&gpu.entry, &gpu.instance);
                if !surface_fns
                    .get_physical_device_surface_support(gpu.pd, gpu.family, surface)
                    .map_err(err)?
                {
                    return Err("unavailable: Xlib graphics/present queue".into());
                }
                let caps = surface_fns
                    .get_physical_device_surface_capabilities(gpu.pd, surface)
                    .map_err(err)?;
                let formats = surface_fns
                    .get_physical_device_surface_formats(gpu.pd, surface)
                    .map_err(err)?;
                let format = *formats
                    .iter()
                    .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
                    .or_else(|| {
                        formats
                            .iter()
                            .find(|f| f.format == vk::Format::B8G8R8A8_SRGB)
                    })
                    .ok_or("unavailable: BGRA surface")?;
                let count = caps.min_image_count.max(3);
                if caps.max_image_count != 0 && count > caps.max_image_count {
                    return Err("unavailable: three swapchain images".into());
                }
                let usage =
                    vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT;
                if !caps.supported_usage_flags.contains(usage) {
                    return Err("unavailable: surface transfer/color usage".into());
                }
                let extent = if caps.current_extent.width == u32::MAX {
                    vk::Extent2D {
                        width: window
                            .width
                            .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                        height: window
                            .height
                            .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                    }
                } else {
                    caps.current_extent
                };
                let fns = ash::khr::swapchain::Device::new(&gpu.instance, &gpu.device);
                let mut s = Self {
                    device: gpu.device.clone(),
                    life: gpu.ctx.life.clone(),
                    fns,
                    swap: vk::SwapchainKHR::null(),
                    images: vec![],
                    initialized: vec![],
                    slots: vec![],
                    pool: gpu.pool,
                    next: 0,
                };
                let composite = [
                    vk::CompositeAlphaFlagsKHR::OPAQUE,
                    vk::CompositeAlphaFlagsKHR::INHERIT,
                ]
                .into_iter()
                .find(|f| caps.supported_composite_alpha.contains(*f))
                .ok_or("surface alpha")?;
                s.swap = s
                    .fns
                    .create_swapchain(
                        &vk::SwapchainCreateInfoKHR::default()
                            .surface(surface)
                            .min_image_count(count)
                            .image_format(format.format)
                            .image_color_space(format.color_space)
                            .image_extent(extent)
                            .image_array_layers(1)
                            .image_usage(usage)
                            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                            .pre_transform(caps.current_transform)
                            .composite_alpha(composite)
                            .present_mode(vk::PresentModeKHR::FIFO)
                            .clipped(true)
                            .old_swapchain(old),
                        None,
                    )
                    .map_err(err)?;
                s.images = s.fns.get_swapchain_images(s.swap).map_err(err)?;
                s.initialized = vec![false; s.images.len()];
                for _ in 0..3 {
                    let cmd = s
                        .device
                        .allocate_command_buffers(
                            &vk::CommandBufferAllocateInfo::default()
                                .command_pool(gpu.pool)
                                .level(vk::CommandBufferLevel::PRIMARY)
                                .command_buffer_count(1),
                        )
                        .map_err(err)?[0];
                    let fence = s
                        .device
                        .create_fence(
                            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                            None,
                        )
                        .map_err(err)?;
                    let acquire = s
                        .device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        .map_err(err)?;
                    s.slots.push(Slot {
                        cmd,
                        fence,
                        acquire,
                    });
                }
                Ok(s)
            }
        }
        fn frame(&mut self, gpu: &mut gpu::Gpu) -> Result<bool, String> {
            let slot = &self.slots[self.next % self.slots.len()];
            self.next += 1;
            gpu.wait(slot.fence)?;
            // SAFETY: acquired image and this command slot remain owned through
            // the bounded rendering fence before this probe presents the image.
            unsafe {
                let acquired = self.fns.acquire_next_image(
                    self.swap,
                    2_000_000_000,
                    slot.acquire,
                    vk::Fence::null(),
                );
                let (index, suboptimal) = match acquired {
                    Ok(v) => v,
                    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
                    Err(e) => return Err(err(e)),
                };
                let i = index as usize;
                self.device
                    .reset_command_buffer(slot.cmd, vk::CommandBufferResetFlags::empty())
                    .map_err(err)?;
                self.device
                    .begin_command_buffer(
                        slot.cmd,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(err)?;
                let old = if self.initialized[i] {
                    vk::ImageLayout::PRESENT_SRC_KHR
                } else {
                    vk::ImageLayout::UNDEFINED
                };
                let barrier = |old, new, src, dst| {
                    vk::ImageMemoryBarrier::default()
                        .old_layout(old)
                        .new_layout(new)
                        .src_access_mask(src)
                        .dst_access_mask(dst)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(self.images[i])
                        .subresource_range(resources::COLOR_RANGE)
                };
                self.device.cmd_pipeline_barrier(
                    slot.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier(
                        old,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::TRANSFER_WRITE,
                    )],
                );
                self.device.cmd_clear_color_image(
                    slot.cmd,
                    self.images[i],
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [0.1, 0.2, 0.3, 1.0],
                    },
                    &[resources::COLOR_RANGE],
                );
                self.device.cmd_pipeline_barrier(
                    slot.cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier(
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::PRESENT_SRC_KHR,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::empty(),
                    )],
                );
                self.device.end_command_buffer(slot.cmd).map_err(err)?;
                let waits = [slot.acquire];
                let stages = [vk::PipelineStageFlags::TRANSFER];
                let cmds = [slot.cmd];
                let submit = vk::SubmitInfo::default()
                    .wait_semaphores(&waits)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds);
                self.device.reset_fences(&[slot.fence]).map_err(err)?;
                if let Err(e) = self.device.queue_submit(gpu.queue, &[submit], slot.fence) {
                    gpu.destroy();
                    return Err(err(e));
                }
                self.initialized[i] = true;
                // Probe-only host ordering: no game-owned semaphore payload is
                // lent to WSI. Reacquiring every image is not a fairness guarantee.
                // The production layer still submits/signals its own binary wait
                // and retires that presentation with the actual maintenance fence.
                gpu.wait(slot.fence)?;
                let swaps = [self.swap];
                let indices = [index];
                let present = vk::PresentInfoKHR::default()
                    .swapchains(&swaps)
                    .image_indices(&indices);
                match self.fns.queue_present(gpu.queue, &present) {
                    Ok(sub) => Ok(!suboptimal && !sub),
                    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(false),
                    Err(e) => Err(err(e)),
                }
            }
        }
        fn drain(&mut self, gpu: &mut gpu::Gpu) -> Result<(), String> {
            let cmd = gpu.command(|_| Ok(()))?;
            gpu.submit(cmd, &[])?;
            for slot in &self.slots {
                gpu.wait(slot.fence)?;
            }
            Ok(())
        }
    }
    impl Drop for Swap {
        fn drop(&mut self) {
            if !self.life.is_live() {
                return;
            }
            // SAFETY: the probe fences each rendering submission before
            // presentation, and drain fences all queued layer submissions. There
            // are no probe-owned present wait semaphores to retire by reacquisition.
            unsafe {
                for slot in self.slots.drain(..) {
                    self.device.destroy_fence(slot.fence, None);
                    self.device.destroy_semaphore(slot.acquire, None);
                    self.device.free_command_buffers(self.pool, &[slot.cmd]);
                }
                self.fns.destroy_swapchain(self.swap, None);
            }
        }
    }
    pub fn run() -> Result<serde_json::Value, String> {
        let mut max_frames = 600;
        let (mut width, mut height) = (320, 240);
        let args: Vec<_> = std::env::args().collect();
        for p in args.windows(2) {
            if p[0] == "--frames" {
                max_frames = p[1].parse::<u64>().map_err(err)?.min(100000);
            }
            if p[0] == "--width" {
                width = p[1].parse::<u32>().map_err(err)?.clamp(64, 1024);
            }
            if p[0] == "--height" {
                height = p[1].parse::<u32>().map_err(err)?.clamp(64, 1024);
            }
        }
        let identity = uw_frame_channel::PeerIdentity::current().map_err(err)?;
        println!("IDENTITY {} {}", identity.pid, identity.start_ticks);
        let mut window = x11::Window::new(width, height)?;
        let mut gpu = gpu::Gpu::new(true)?;
        // SAFETY: owned mapped Xlib window remains live until after the surface.
        let surface = unsafe {
            ash::khr::xlib_surface::Instance::new(&gpu.entry, &gpu.instance).create_xlib_surface(
                &vk::XlibSurfaceCreateInfoKHR::default()
                    .dpy(window.display.cast())
                    .window(window.window),
                None,
            )
        }
        .map_err(err)?;
        let mut swap = Swap::new(&gpu, surface, &window, vk::SwapchainKHR::null())?;
        println!("READY {}", swap.images.len());
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.len() > 256 {
                    break;
                }
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut frames = 0;
        let mut resizes = 0;
        let mut paused = false;
        let started = Instant::now();
        let mut quit = false;
        let mut suboptimal = 0;
        while frames < max_frames && !quit && started.elapsed() < Duration::from_secs(120) {
            for command in rx.try_iter() {
                let words: Vec<_> = command.split_whitespace().collect();
                match words.as_slice() {
                    ["quit"] => quit = true,
                    ["pause"] => {
                        swap.drain(&mut gpu)?;
                        paused = true;
                        println!("PAUSED");
                    }
                    ["resume"] => {
                        paused = false;
                        println!("RESUMED");
                    }
                    ["resize", width, height] => {
                        let w = width.parse::<u32>().map_err(err)?.clamp(64, 1024);
                        let h = height.parse::<u32>().map_err(err)?.clamp(64, 1024);
                        swap.drain(&mut gpu)?;
                        window.resize(w, h);
                        let new = Swap::new(&gpu, surface, &window, swap.swap)?;
                        let old = std::mem::replace(&mut swap, new);
                        drop(old);
                        resizes += 1;
                        println!("RESIZED {w} {h}");
                    }
                    _ => {}
                }
            }
            if paused {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            if !swap.frame(&mut gpu)? {
                suboptimal += 1;
                swap.drain(&mut gpu)?;
                let new = Swap::new(&gpu, surface, &window, swap.swap)?;
                drop(std::mem::replace(&mut swap, new));
            }
            frames += 1;
        }
        swap.drain(&mut gpu)?;
        drop(swap);
        // SAFETY: all application swapchains were drained and destroyed.
        unsafe {
            ash::khr::surface::Instance::new(&gpu.entry, &gpu.instance)
                .destroy_surface(surface, None);
        }
        let mut report = gpu.report();
        report["frames"] = frames.into();
        report["resizes"] = resizes.into();
        report["suboptimalOrOutOfDate"] = suboptimal.into();
        drop(gpu);
        let errors = gpu::validation_errors();
        report["validationErrors"] = errors.into();
        if errors != 0 {
            return Err(format!("{errors} WSI validation errors"));
        }
        report["status"] = "pass".into();
        Ok(report)
    }
}
#[cfg(target_os = "linux")]
fn main() {
    match wsi::run() {
        Ok(report) => println!("REPORT {report}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    std::process::exit(2)
}
