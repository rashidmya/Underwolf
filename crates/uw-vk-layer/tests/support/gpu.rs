//! Owned offscreen driver harness. Imports/recording/pipelines/fence completion
//! compile the exact production sources; no alternate compositor implementation.
#![allow(dead_code)]
#[path = "../../src/draw.rs"]
pub(crate) mod draw;
#[path = "../../src/lifetime.rs"]
pub(crate) mod lifetime;
#[path = "../../src/loader_abi.rs"]
pub(crate) mod loader_abi;
#[path = "../../src/resources.rs"]
pub(crate) mod resources;
use ash::vk;
use resources::{COLOR_RANGE, Context, Texture, memory_type};
use std::result::Result;
use std::{
    ffi::{CStr, c_void},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uw_frame_channel::{
    FrameMapping, Patch, PeerIdentity, PublishResult, ReadLease, live_mapping_count,
};
use uw_frame_proto::*;
static SERIAL: Mutex<()> = Mutex::new(());
static ERRORS: AtomicU64 = AtomicU64::new(0);
unsafe extern "system" fn debug(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _: *mut c_void,
) -> vk::Bool32 {
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        ERRORS.fetch_add(1, Ordering::SeqCst);
    }
    // SAFETY: callback data/message are valid for the duration of this invocation.
    unsafe {
        if !data.is_null() && !(*data).p_message.is_null() {
            eprintln!(
                "VALIDATION {:?}: {}",
                severity,
                CStr::from_ptr((*data).p_message).to_string_lossy()
            );
        }
    }
    vk::FALSE
}
pub struct Gpu {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub device: ash::Device,
    pub pd: vk::PhysicalDevice,
    pub family: u32,
    pub queue: vk::Queue,
    pub ctx: Arc<Context>,
    pub pool: vk::CommandPool,
    pub debug_utils: ash::ext::debug_utils::Instance,
    pub messenger: vk::DebugUtilsMessengerEXT,
    pub validation_version: u32,
    pub name: String,
    pub driver: u32,
    pub uuid: String,
    dead: bool,
}
fn err(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}
impl Gpu {
    pub fn new(wsi: bool) -> Result<Self, String> {
        // SAFETY: all instance/device structures own their backing arrays until
        // the driver returns; queried extensions/features precede every enable.
        unsafe {
            let entry = ash::Entry::load().map_err(err)?;
            let validation=entry.enumerate_instance_layer_properties().map_err(err)?.into_iter().find(|p|p.layer_name_as_c_str()==Ok(c"VK_LAYER_KHRONOS_validation")).ok_or("unavailable: VK_LAYER_KHRONOS_validation; supply process-scoped VK_ADD_LAYER_PATH")?;
            let mut exts = vec![
                ash::ext::debug_utils::NAME.as_ptr(),
                ash::ext::validation_features::NAME.as_ptr(),
                ash::khr::surface::NAME.as_ptr(),
                ash::ext::surface_maintenance1::NAME.as_ptr(),
                ash::khr::get_surface_capabilities2::NAME.as_ptr(),
            ];
            if wsi && std::env::var("UW_PROBE_OMIT_MAINTENANCE_INSTANCE").as_deref() == Ok("1") {
                exts.retain(|p| {
                    *p != ash::ext::surface_maintenance1::NAME.as_ptr()
                        && *p != ash::khr::get_surface_capabilities2::NAME.as_ptr()
                });
            }
            if wsi {
                exts.push(ash::khr::xlib_surface::NAME.as_ptr());
            }
            let mut layers = vec![];
            if wsi
                && std::env::var("UW_LAYER_ENABLE").as_deref() == Ok("1")
                && std::env::var("UW_LAYER_DISABLE").as_deref() != Ok("1")
            {
                layers.push(c"VK_LAYER_UNDERWOLF_overlay".as_ptr());
            }
            layers.push(c"VK_LAYER_KHRONOS_validation".as_ptr());
            let app = vk::ApplicationInfo::default()
                .application_name(c"Underwolf rendering probe")
                .api_version(vk::API_VERSION_1_3);
            let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
            let mut validation_features =
                vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
            let mut debug_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(debug));
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default()
                        .application_info(&app)
                        .enabled_layer_names(&layers)
                        .enabled_extension_names(&exts)
                        .push_next(&mut validation_features)
                        .push_next(&mut debug_info),
                    None,
                )
                .map_err(err)?;
            debug_info.p_next = std::ptr::null();
            let debug_utils = ash::ext::debug_utils::Instance::new(&entry, &instance);
            let messenger = debug_utils
                .create_debug_utils_messenger(&debug_info, None)
                .map_err(err)?;
            let devices = instance.enumerate_physical_devices().map_err(err)?;
            let pd = devices
                .into_iter()
                .find(|pd| {
                    instance.get_physical_device_properties(*pd).device_type
                        == vk::PhysicalDeviceType::DISCRETE_GPU
                })
                .ok_or("unavailable: discrete GPU")?;
            let props = instance.get_physical_device_properties(pd);
            let name = CStr::from_ptr(props.device_name.as_ptr())
                .to_string_lossy()
                .into_owned();
            let mut id = vk::PhysicalDeviceIDProperties::default();
            let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
            let mut query = vk::PhysicalDeviceProperties2::default()
                .push_next(&mut id)
                .push_next(&mut host);
            instance.get_physical_device_properties2(pd, &mut query);
            let uuid = id.device_uuid.iter().map(|b| format!("{b:02x}")).collect();
            let family = instance
                .get_physical_device_queue_family_properties(pd)
                .iter()
                .position(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .ok_or("unavailable: graphics queue")? as u32;
            let available = instance
                .enumerate_device_extension_properties(pd)
                .map_err(err)?;
            let wanted = [
                ash::ext::external_memory_host::NAME,
                ash::khr::swapchain::NAME,
                ash::ext::swapchain_maintenance1::NAME,
            ];
            if !wanted.iter().all(|n| {
                available
                    .iter()
                    .any(|p| p.extension_name_as_c_str() == Ok(*n))
            }) {
                return Err("unavailable: host import/present maintenance extension".into());
            }
            let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default();
            let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
            let mut features = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut maintenance)
                .push_next(&mut timeline);
            instance.get_physical_device_features2(pd, &mut features);
            if timeline.timeline_semaphore != vk::TRUE {
                return Err("unavailable: probe timeline semaphore".into());
            }
            if maintenance.swapchain_maintenance1 != vk::TRUE {
                return Err("unavailable: swapchainMaintenance1 feature".into());
            }
            maintenance.p_next = std::ptr::null_mut();
            timeline.p_next = std::ptr::null_mut();
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let extensions: Vec<_> = wanted.iter().map(|n| n.as_ptr()).collect();
            let device = instance
                .create_device(
                    pd,
                    &vk::DeviceCreateInfo::default()
                        .queue_create_infos(&queues)
                        .enabled_extension_names(&extensions)
                        .push_next(&mut maintenance)
                        .push_next(&mut timeline),
                    None,
                )
                .map_err(err)?;
            let queue = device.get_device_queue(family, 0);
            let host_fn = ash::ext::external_memory_host::DeviceFn::load(|name| {
                instance
                    .get_device_proc_addr(device.handle(), name.as_ptr())
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            });
            let ctx = Context::new(
                device.clone(),
                host_fn,
                instance.get_physical_device_memory_properties(pd),
                host.min_imported_host_pointer_alignment.max(4096),
            )
            .map_err(err)?;
            let pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(err)?;
            Ok(Self {
                entry,
                instance,
                device,
                pd,
                family,
                queue,
                ctx,
                pool,
                debug_utils,
                messenger,
                validation_version: validation.spec_version,
                name,
                driver: props.driver_version,
                uuid,
                dead: false,
            })
        }
    }
    pub fn wait(&mut self, fence: vk::Fence) -> Result<(), String> {
        // SAFETY: own live submission fence; five-second failure bound, no idle wait.
        let result = unsafe { self.device.wait_for_fences(&[fence], true, 5_000_000_000) };
        if let Err(e) = result {
            self.destroy();
            return Err(format!("bounded fence wait failed: {e:?}"));
        }
        Ok(())
    }
    pub fn command(
        &self,
        record: impl FnOnce(vk::CommandBuffer) -> Result<(), String>,
    ) -> Result<vk::CommandBuffer, String> {
        // SAFETY: private command pool; local recording owns all references.
        unsafe {
            let cmd = self
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(err)?[0];
            self.device
                .begin_command_buffer(
                    cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(err)?;
            record(cmd)?;
            self.device.end_command_buffer(cmd).map_err(err)?;
            Ok(cmd)
        }
    }
    pub fn submit(&mut self, cmd: vk::CommandBuffer, wait: &[vk::Semaphore]) -> Result<(), String> {
        // SAFETY: private queue and recorded command; semaphore waits and fence
        // remain owned until the bounded completion wait returns.
        unsafe {
            let fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(err)?;
            let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; wait.len()];
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default()
                .command_buffers(&cmds)
                .wait_semaphores(wait)
                .wait_dst_stage_mask(&stages);
            if let Err(e) = self.device.queue_submit(self.queue, &[submit], fence) {
                self.destroy();
                return Err(err(e));
            }
            self.wait(fence)?;
            self.device.destroy_fence(fence, None);
            self.device.free_command_buffers(self.pool, &cmds);
        }
        Ok(())
    }
    pub fn destroy(&mut self) {
        if self.dead {
            return;
        }
        if !self.ctx.life.is_live() {
            self.dead = true;
            return;
        }
        self.dead = true;
        // SAFETY: failure teardown retains all callers' objects/mappings through
        // this call; their later drops suppress calls on the destroyed device.
        unsafe {
            self.device.destroy_device(None);
        }
        self.ctx.life.live.store(false, Ordering::Release);
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"device":self.name,"driverRaw":self.driver,"deviceUuid":self.uuid,"validationVersion":self.validation_version,"validationErrors":ERRORS.load(Ordering::SeqCst),"alignment":self.ctx.alignment})
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        if !self.dead && self.ctx.life.is_live() {
            // SAFETY: each owned probe submission completed its bounded fence before
            // normal return, all child textures/targets/renderers have been dropped.
            unsafe {
                self.device.destroy_command_pool(self.pool, None);
                self.ctx.destroy_handles();
            }
        }
        self.destroy();
        // SAFETY: device has returned from destruction; messenger/instance are owned.
        unsafe {
            self.debug_utils
                .destroy_debug_utils_messenger(self.messenger, None);
            self.instance.destroy_instance(None);
        }
    }
}
struct Target {
    ctx: Arc<Context>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    buffer: vk::Buffer,
    read_memory: vk::DeviceMemory,
    width: u32,
    height: u32,
    first: bool,
}
impl Target {
    fn new(gpu: &Gpu, width: u32, height: u32, srgb: bool) -> Result<Self, String> {
        let mut t = Self {
            ctx: gpu.ctx.clone(),
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            buffer: vk::Buffer::null(),
            read_memory: vk::DeviceMemory::null(),
            width,
            height,
            first: true,
        };
        // SAFETY: private target/readback objects, checked memory types/extents.
        unsafe {
            let d = &gpu.device;
            t.image = d
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(if srgb {
                            vk::Format::B8G8R8A8_SRGB
                        } else {
                            vk::Format::B8G8R8A8_UNORM
                        })
                        .extent(vk::Extent3D {
                            width,
                            height,
                            depth: 1,
                        })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(
                            vk::ImageUsageFlags::COLOR_ATTACHMENT
                                | vk::ImageUsageFlags::TRANSFER_SRC
                                | vk::ImageUsageFlags::TRANSFER_DST,
                        ),
                    None,
                )
                .map_err(err)?;
            let req = d.get_image_memory_requirements(t.image);
            let mt = memory_type(
                &gpu.ctx.props,
                req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .ok_or("target memory")?;
            t.memory = d
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(req.size)
                        .memory_type_index(mt),
                    None,
                )
                .map_err(err)?;
            d.bind_image_memory(t.image, t.memory, 0).map_err(err)?;
            t.buffer = d
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(u64::from(width) * u64::from(height) * 4)
                        .usage(vk::BufferUsageFlags::TRANSFER_DST),
                    None,
                )
                .map_err(err)?;
            let req = d.get_buffer_memory_requirements(t.buffer);
            let mt = memory_type(
                &gpu.ctx.props,
                req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or("readback memory")?;
            t.read_memory = d
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(req.size)
                        .memory_type_index(mt),
                    None,
                )
                .map_err(err)?;
            d.bind_buffer_memory(t.buffer, t.read_memory, 0)
                .map_err(err)?;
        }
        Ok(t)
    }
    fn clear(&mut self, gpu: &mut Gpu) -> Result<(), String> {
        let cmd = gpu.command(|cmd| {
            // SAFETY: target's previous draw/readback is complete on this private
            // queue. Transitions preserve the next renderer's attachment contract.
            unsafe {
                let old = if self.first {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
                };
                let barrier = target_barrier(
                    self.image,
                    old,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                    vk::AccessFlags::TRANSFER_WRITE,
                );
                gpu.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                );
                gpu.device.cmd_clear_color_image(
                    cmd,
                    self.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue { float32: [1.0; 4] },
                    &[COLOR_RANGE],
                );
                gpu.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[target_barrier(
                        self.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::COLOR_ATTACHMENT_READ
                            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    )],
                );
            }
            Ok(())
        })?;
        gpu.submit(cmd, &[])?;
        self.first = false;
        Ok(())
    }
    fn read(&self, gpu: &mut Gpu, wait: &[vk::Semaphore]) -> Result<Vec<u8>, String> {
        let cmd = gpu.command(|cmd| {
            // SAFETY: renderer signal orders color writes before transfer read; host
            // visibility follows the copy and its actual bounded submission fence.
            unsafe {
                gpu.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[target_barrier(
                        self.image,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        vk::AccessFlags::TRANSFER_READ,
                    )],
                );
                let copy = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: self.width,
                        height: self.height,
                        depth: 1,
                    });
                gpu.device.cmd_copy_image_to_buffer(
                    cmd,
                    self.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    self.buffer,
                    &[copy],
                );
                let host = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(self.buffer)
                    .size(vk::WHOLE_SIZE);
                gpu.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[host],
                    &[target_barrier(
                        self.image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags::TRANSFER_READ,
                        vk::AccessFlags::COLOR_ATTACHMENT_READ
                            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    )],
                );
            }
            Ok(())
        })?;
        gpu.submit(cmd, wait)?;
        // SAFETY: coherent memory is fully written by the completed copy.
        unsafe {
            let len = (self.width * self.height * 4) as usize;
            let ptr = gpu
                .device
                .map_memory(self.read_memory, 0, len as u64, vk::MemoryMapFlags::empty())
                .map_err(err)?;
            let bytes = std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec();
            gpu.device.unmap_memory(self.read_memory);
            Ok(bytes)
        }
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        if !self.ctx.life.is_live() {
            return;
        }
        // SAFETY: all target commands are drained by bounded fences before drop.
        unsafe {
            let d = &self.ctx.life.device;
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
            d.destroy_buffer(self.buffer, None);
            d.free_memory(self.read_memory, None);
        }
    }
}
fn target_barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: vk::AccessFlags,
    dst: vk::AccessFlags,
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .src_access_mask(src)
        .dst_access_mask(dst)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(COLOR_RANGE)
}
unsafe extern "system" fn no_loader(_: vk::Device, _: *mut c_void) -> vk::Result {
    vk::Result::SUCCESS
}
fn mapping(
    gpu: &Gpu,
    generation: u64,
    width: u32,
    height: u32,
) -> Result<Arc<FrameMapping>, String> {
    let key = AllocationKey {
        session: SessionId([7; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 1,
        },
        generation,
    };
    Ok(Arc::new(
        FrameMapping::create(
            key,
            FrameLayout::new(width, height, gpu.ctx.alignment).map_err(err)?,
            PeerIdentity::current().map_err(err)?,
        )
        .map_err(err)?,
    ))
}
fn full(mapping: &FrameMapping) -> Rect {
    let l = mapping.header().layout;
    Rect {
        x: 0,
        y: 0,
        width: l.width,
        height: l.height,
    }
}
fn publish(mapping: &FrameMapping, seq: u64, rect: Rect, pixels: &[u8]) -> Result<(), String> {
    let result = mapping
        .try_publish(
            seq,
            Patch {
                rect,
                stride: rect.width * 4,
                data: pixels,
            },
        )
        .map_err(err)?;
    if !matches!(result, PublishResult::Published { .. }) {
        return Err(format!("unexpected publish: {result:?}"));
    }
    Ok(())
}
fn expected(pixels: &[u8], srgb: bool) -> Vec<u8> {
    pixels
        .chunks_exact(4)
        .flat_map(|p| {
            let alpha = f64::from(p[3]) / 255.0;
            let mut result = [255u8; 4];
            for i in 0..3 {
                let x = f64::from(p[i]) / 255.0;
                let linear = if srgb {
                    if x <= 0.04045 {
                        x / 12.92
                    } else {
                        ((x + 0.055) / 1.055).powf(2.4)
                    }
                } else {
                    x
                };
                let composite = (linear + 1.0 - alpha).clamp(0.0, 1.0);
                let out = if srgb {
                    if composite <= 0.0031308 {
                        12.92 * composite
                    } else {
                        1.055 * composite.powf(1.0 / 2.4) - 0.055
                    }
                } else {
                    composite
                };
                result[i] = (out * 255.0).round() as u8;
            }
            result
        })
        .collect()
}
fn compare(actual: &[u8], pixels: &[u8], srgb: bool) -> Result<(), String> {
    let expected = expected(pixels, srgb);
    let tolerance = if srgb { 1 } else { 0 };
    if let Some((i, (a, b))) = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .find(|(i, (a, b))| a.abs_diff(**b) > if i % 4 == 3 { 0 } else { tolerance })
    {
        return Err(format!(
            "pixel byte {i}: got {a}, expected {b}, srgb={srgb}"
        ));
    }
    Ok(())
}
fn draw_frame(
    gpu: &mut Gpu,
    target: &mut Target,
    renderer: &mut draw::Renderer,
    texture: &Arc<Texture>,
    lease: Option<ReadLease>,
    first: bool,
) -> Result<Vec<u8>, String> {
    target.clear(gpu)?;
    let rect = full(texture.mapping.as_ref());
    let quads = [draw::Quad {
        rect,
        x: 0,
        y: 0,
        texture: Some(texture.clone()),
        color: [0.0; 4],
    }];
    let uploads: Vec<_> = lease
        .as_ref()
        .map(|l| draw::Upload {
            texture,
            damage: l.claim().damage,
            first,
        })
        .into_iter()
        .collect();
    // SAFETY: private graphics queue/target, completed previous frame, immutable
    // claimed pixels and retained assets; actual completion is polled below.
    let outcome = unsafe { renderer.draw(gpu.queue, gpu.family, 0, &[], &quads, &uploads, 1) };
    if !matches!(outcome, crate::DrawOutcome::Submitted { .. }) {
        gpu.destroy();
        return Err(format!("offscreen draw {outcome:?}"));
    }
    renderer.frames[0].assets.push(texture.clone());
    if let Some(lease) = lease {
        renderer.frames[0].leases.push(lease);
    }
    let bytes = target.read(gpu, &[renderer.frames[0].semaphore])?;
    // SAFETY: readback fence is after renderer's signal and therefore proves all
    // reads; production fence polling performs the actual shared-state update.
    unsafe { renderer.frames[0].poll() }.map_err(err)?;
    Ok(bytes)
}
pub fn run(scenario: &str) -> Result<serde_json::Value, String> {
    let _serial = SERIAL.lock().unwrap();
    ERRORS.store(0, Ordering::SeqCst);
    let mut gpu = Gpu::new(false)?;
    let before = live_mapping_count();
    let srgb = scenario == "srgb";
    match scenario {
        "unorm" | "srgb" => pixels(&mut gpu, srgb)?,
        "held" => held(&mut gpu)?,
        "churn" => churn(&mut gpu)?,
        _ => return Err("unknown scenario".into()),
    };
    if live_mapping_count() != before {
        return Err("mapping count did not return to baseline".into());
    }
    if gpu.ctx.counts.snapshot() != (0, 0, 0) || gpu.ctx.budget.used() != 0 {
        return Err(format!(
            "GPU resources did not return to baseline: {:?}, {}",
            gpu.ctx.counts.snapshot(),
            gpu.ctx.budget.used()
        ));
    }
    let mut report = gpu.report();
    report["status"] = "pass".into();
    report["scenario"] = scenario.into();
    if scenario == "held" {
        report["realChannelDisconnect"] = true.into();
        report["delayPrimitive"] = "offscreen host-signalled timeline gate".into();
    }
    report["mappingDelta"] = 0.into();
    report["resourceCounts"] = serde_json::json!({"imports":0,"images":0,"descriptors":0});
    report["srgbTolerance"] = if srgb { 1 } else { 0 }.into();
    report["toleranceScope"] = "sRGB colour conversion only; alpha exact".into();
    drop(gpu);
    if ERRORS.load(Ordering::SeqCst) != 0 {
        return Err(format!(
            "{} validation errors",
            ERRORS.load(Ordering::SeqCst)
        ));
    }
    Ok(report)
}
fn pixels(gpu: &mut Gpu, srgb: bool) -> Result<(), String> {
    let mapping = mapping(gpu, 1, 16, 8)?;
    let mut pixels: Vec<u8> = (0..128)
        .flat_map(|i| {
            if (i % 16 / 2 + i / 16 / 2) % 2 == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 255, 255]
            }
        })
        .collect();
    publish(&mapping, 1, full(&mapping), &pixels)?;
    let lease = mapping.try_claim().map_err(err)?.unwrap();
    // SAFETY: claimed native pixels and private live device; all GPU use drained.
    let texture = unsafe { Texture::new(gpu.ctx.clone(), &lease) }.map_err(err)?;
    let mut target = Target::new(gpu, 16, 8, srgb)?;
    target.clear(gpu)?;
    // SAFETY: owned offscreen target has the declared color attachment layout.
    let mut renderer = unsafe {
        draw::Renderer::new_offscreen(
            gpu.ctx.clone(),
            no_loader,
            if srgb {
                vk::Format::B8G8R8A8_SRGB
            } else {
                vk::Format::B8G8R8A8_UNORM
            },
            vk::Extent2D {
                width: 16,
                height: 8,
            },
            target.image,
        )
    }
    .map_err(err)?;
    compare(
        &draw_frame(gpu, &mut target, &mut renderer, &texture, Some(lease), true)?,
        &pixels,
        srgb,
    )?;
    for (seq, rect, color) in [
        (
            2,
            Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            [0, 0, 128, 128],
        ),
        (
            3,
            Rect {
                x: 12,
                y: 5,
                width: 2,
                height: 2,
            },
            [64, 0, 0, 128],
        ),
        (
            4,
            Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            [0, 0, 0, 0],
        ),
    ] {
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                let i = ((y * 16 + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&color);
            }
        }
        let patch: Vec<_> = (0..rect.width * rect.height).flat_map(|_| color).collect();
        publish(&mapping, seq, rect, &patch)?;
        let lease = mapping.try_claim().map_err(err)?.unwrap();
        compare(
            &draw_frame(
                gpu,
                &mut target,
                &mut renderer,
                &texture,
                Some(lease),
                false,
            )?,
            &pixels,
            srgb,
        )?;
    }
    // No new upload: the retained complete image still composites correctly.
    compare(
        &draw_frame(gpu, &mut target, &mut renderer, &texture, None, false)?,
        &pixels,
        srgb,
    )?;
    Ok(())
}
struct Ipc {
    producer: uw_render_bridge::ProducerSession,
    stream: std::os::unix::net::UnixStream,
    path: std::path::PathBuf,
    root: std::path::PathBuf,
    alignment: u64,
}
impl Ipc {
    fn new(alignment: u64) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!(
            "uw-gpu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(err)?
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(err)?;
        let path = root.join("render.sock");
        let (producer, stream) = Self::open(&path, alignment)?;
        Ok(Self {
            producer,
            stream,
            path,
            root,
            alignment,
        })
    }
    fn receive(
        stream: &std::os::unix::net::UnixStream,
    ) -> Result<(Message, Vec<std::os::fd::OwnedFd>), String> {
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match uw_frame_channel::recv_message(stream) {
                Err(FrameError::Busy) if std::time::Instant::now() < until => {}
                other => return other.map_err(err),
            }
        }
    }
    fn open(
        path: &std::path::Path,
        alignment: u64,
    ) -> Result<
        (
            uw_render_bridge::ProducerSession,
            std::os::unix::net::UnixStream,
        ),
        String,
    > {
        use uw_frame_channel::send_message;
        let peer = PeerIdentity::current().map_err(err)?;
        let producer = uw_render_bridge::ProducerSession::bind_path(path, peer).map_err(err)?;
        let stream = std::os::unix::net::UnixStream::connect(path).map_err(err)?;
        send_message(
            &stream,
            &Message::Hello {
                pid: peer.pid,
                start_ticks: peer.start_ticks,
                alignment,
                mutex_size: 40,
                mutex_align: 8,
                abi: 1,
                capability: 1,
            },
            &[],
        )
        .map_err(err)?;
        let (welcome, fds) = Self::receive(&stream)?;
        if !matches!(welcome, Message::Welcome { .. }) || !fds.is_empty() {
            return Err("IPC welcome".into());
        }
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if producer
                .drain_events()
                .iter()
                .any(|e| matches!(e, uw_render_bridge::BridgeEvent::Ready { .. }))
            {
                break;
            }
            if std::time::Instant::now() >= until {
                return Err("IPC producer ready timeout".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok((producer, stream))
    }
    fn create(&self, width: u32, height: u32) -> Result<Arc<FrameMapping>, String> {
        let key = self
            .producer
            .create_widget(
                WidgetKey {
                    id: 1,
                    generation: 1,
                },
                width,
                height,
            )
            .map_err(err)?;
        loop {
            let (m, fds) = Self::receive(&self.stream)?;
            if let Message::Register(header) = m {
                if header.key != key {
                    return Err("IPC stale register".into());
                }
                // SAFETY: our real native producer initialized the metadata/mutex. The
                // consumer preserves the shared ownership ABI and retains imported bytes.
                let mapping = Arc::new(
                    unsafe {
                        FrameMapping::open(
                            &header.encode().map_err(err)?,
                            fds.try_into().map_err(|_| "IPC descriptor count")?,
                            PeerIdentity::from_stream(&self.stream).map_err(err)?,
                        )
                    }
                    .map_err(err)?,
                );
                uw_frame_channel::send_message(&self.stream, &Message::Registered { key }, &[])
                    .map_err(err)?;
                return Ok(mapping);
            }
        }
    }
    fn restart(&mut self) -> Result<(), String> {
        self.producer.stop();
        let (producer, stream) = Self::open(&self.path, self.alignment)?;
        self.producer = producer;
        self.stream = stream;
        Ok(())
    }
    fn publish_ready(
        &self,
        mapping: &FrameMapping,
        seq: u64,
        rect: Rect,
        bytes: &[u8],
    ) -> Result<PublishResult, String> {
        let until = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let result = self.publish(mapping, seq, rect, bytes)?;
            if !matches!(result, PublishResult::Busy) {
                return Ok(result);
            }
            if std::time::Instant::now() >= until {
                return Err("publication busy deadline".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn publish(
        &self,
        mapping: &FrameMapping,
        seq: u64,
        rect: Rect,
        bytes: &[u8],
    ) -> Result<PublishResult, String> {
        self.producer
            .publish_patch(
                mapping.header().key,
                seq,
                Patch {
                    rect,
                    stride: rect.width * 4,
                    data: bytes,
                },
            )
            .map_err(err)
    }
}
impl Drop for Ipc {
    fn drop(&mut self) {
        self.producer.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn held(gpu: &mut Gpu) -> Result<(), String> {
    let mut ipc = Ipc::new(gpu.ctx.alignment)?;
    let mapping = ipc.create(16, 8)?;
    let mut pixels: Vec<u8> = (0..128).flat_map(|_| [0, 0, 255, 255]).collect();
    let publication = ipc.publish_ready(&mapping, 1, full(&mapping), &pixels)?;
    if !matches!(publication, PublishResult::Published { .. }) {
        return Err("IPC baseline busy".into());
    }
    let lease = mapping.try_claim().map_err(err)?.unwrap();
    // SAFETY: this lease owns the native bytes and will be retained through the
    // real fence; the private event always receives its bounded host release.
    let texture = unsafe { Texture::new(gpu.ctx.clone(), &lease) }.map_err(err)?;
    // SAFETY: private host-signallable timeline gate and completion fence.
    let (event, fence) = unsafe {
        let mut ty = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        (
            gpu.device
                .create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty), None)
                .map_err(err)?,
            gpu.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(err)?,
        )
    };
    let cmd = gpu.command(|cmd| {
        // SAFETY: production upload uses the retained claimed mapping; the
        // submission's timeline wait delays its transfer read.
        unsafe {
            draw::record_upload(
                &gpu.device,
                cmd,
                &draw::Upload {
                    texture: &texture,
                    damage: lease.claim().damage,
                    first: true,
                },
            )
            .map_err(err)?;
        }
        Ok(())
    })?;
    let release_device = gpu.device.clone();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        // SAFETY: host timeline signaling while a queue wait is pending is valid;
        // the main thread joins before any device/semaphore teardown.
        unsafe {
            release_device
                .signal_semaphore(&vk::SemaphoreSignalInfo::default().semaphore(event).value(1))
        }
    });
    // SAFETY: private queue; all submission arrays live through queue_submit and
    // the actual upload lease remains retained until its fence completes.
    let submitted = unsafe {
        let waits = [event];
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let cmds = [cmd];
        let values = [1];
        let mut timeline =
            vk::TimelineSemaphoreSubmitInfo::default().wait_semaphore_values(&values);
        gpu.device.queue_submit(
            gpu.queue,
            &[vk::SubmitInfo::default()
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&cmds)
                .push_next(&mut timeline)],
            fence,
        )
    };
    let busy = ipc.publish(
        &mapping,
        2,
        Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        &[0, 255, 0, 255],
    );
    let unchanged = lease
        .copy_pixels()
        .unwrap_or_default()
        .get(..64)
        .is_some_and(|bytes| bytes == &pixels[..64]);
    // One latest static snapshot accumulates both changes while the actual GPU
    // read is outstanding; there will be no subsequent paint to trigger upload.
    pixels[0..4].copy_from_slice(&[0, 255, 0, 255]);
    pixels[508..512].copy_from_slice(&[255, 0, 0, 255]);
    let replacement = (|| -> Result<(Arc<FrameMapping>, Arc<FrameMapping>), String> {
        ipc.restart()?;
        let resized = ipc.create(8, 4)?;
        let first = ipc.publish_ready(&resized, 1, full(&resized), &[0; 128])?;
        if !matches!(first, PublishResult::Published { .. }) {
            return Err("replacement baseline".into());
        }
        let final_mapping = ipc.create(16, 8)?;
        let final_publish = ipc.publish_ready(&final_mapping, 1, full(&final_mapping), &pixels)?;
        if !matches!(final_publish, PublishResult::Published { .. }) {
            return Err("static retained baseline".into());
        }
        Ok((resized, final_mapping))
    })();
    // SAFETY: read-only status query on our own submitted upload fence.
    let was_pending = unsafe { gpu.device.get_fence_status(fence) } == Ok(false);
    let released = release.join();
    let release_error = match released {
        Ok(Ok(())) => None,
        other => {
            // SAFETY: fallback release preserves forward progress even if the owned
            // release thread failed; event/device have not been torn down.
            let fallback = unsafe {
                gpu.device
                    .signal_semaphore(&vk::SemaphoreSignalInfo::default().semaphore(event).value(1))
            };
            if fallback.is_err() {
                Some(format!("timeline release failure: {other:?}"))
            } else {
                None
            }
        }
    };
    if let Err(e) = submitted {
        gpu.destroy();
        return Err(err(e));
    }
    gpu.wait(fence)?;
    let mut frame = lifetime::Frame::new(gpu.ctx.clone());
    frame.fence = fence;
    frame.submitted = true;
    frame.draw_reported = true;
    frame.leases.push(lease);
    frame.assets.push(texture.clone());
    // SAFETY: actual signaled submission fence proves the host-memory GPU read.
    let (completed, _) = unsafe { frame.poll() }.map_err(err)?;
    // SAFETY: event/cmd have no pending use after the verified fence.
    unsafe {
        gpu.device.destroy_semaphore(event, None);
        gpu.device.free_command_buffers(gpu.pool, &[cmd]);
    }
    if let Some(error) = release_error {
        return Err(error);
    }
    let (replacement, final_mapping) = replacement?;
    if !was_pending || replacement.header().key.session == mapping.header().key.session {
        return Err("IPC replacement did not overlap delayed GPU read with a fresh session".into());
    }
    if !matches!(busy, Ok(PublishResult::Busy))
        || !unchanged
        || completed != vec![(mapping.header().key, 1)]
        || replacement.try_state().map_err(err)?.consumed != 0
    {
        return Err("held read ownership/replacement invariant failed".into());
    }
    let final_lease = final_mapping.try_claim().map_err(err)?.unwrap();
    // SAFETY: new generation imports its own immutable baseline; old texture and
    // mapping are still retained independently until this function returns.
    let final_texture = unsafe { Texture::new(gpu.ctx.clone(), &final_lease) }.map_err(err)?;
    let mut target = Target::new(gpu, 16, 8, false)?;
    target.clear(gpu)?;
    // SAFETY: owned offscreen target and serialized completed previous upload.
    let mut renderer = unsafe {
        draw::Renderer::new_offscreen(
            gpu.ctx.clone(),
            no_loader,
            vk::Format::B8G8R8A8_UNORM,
            vk::Extent2D {
                width: 16,
                height: 8,
            },
            target.image,
        )
    }
    .map_err(err)?;
    compare(
        &draw_frame(
            gpu,
            &mut target,
            &mut renderer,
            &final_texture,
            Some(final_lease),
            true,
        )?,
        &pixels,
        false,
    )?;
    Ok(())
}
fn mapping_for(
    gpu: &Gpu,
    key: AllocationKey,
    width: u32,
    height: u32,
) -> Result<Arc<FrameMapping>, String> {
    Ok(Arc::new(
        FrameMapping::create(
            key,
            FrameLayout::new(width, height, gpu.ctx.alignment).map_err(err)?,
            PeerIdentity::current().map_err(err)?,
        )
        .map_err(err)?,
    ))
}
fn churn(gpu: &mut Gpu) -> Result<(), String> {
    let mut target = Target::new(gpu, 16, 8, false)?;
    target.clear(gpu)?;
    // SAFETY: private retained target and one graphics queue; no presentation.
    let mut renderer = unsafe {
        draw::Renderer::new_offscreen(
            gpu.ctx.clone(),
            no_loader,
            vk::Format::B8G8R8A8_UNORM,
            vk::Extent2D {
                width: 16,
                height: 8,
            },
            target.image,
        )
    }
    .map_err(err)?;
    for generation in 1..=100u64 {
        let width = if generation % 4 == 0 { 8 } else { 16 };
        let key = AllocationKey {
            session: SessionId([1 + (generation / 20) as u8; 16]),
            widget: WidgetKey {
                id: 1,
                generation: 1 + (generation / 10) as u32,
            },
            generation,
        };
        let mapping = mapping_for(gpu, key, width, 8)?;
        let color = [generation as u8, 0, 0, 255];
        let pixels: Vec<_> = (0..width * 8).flat_map(|_| color).collect();
        publish(&mapping, 1, full(&mapping), &pixels)?;
        let lease = mapping.try_claim().map_err(err)?.unwrap();
        // SAFETY: new allocation identity always creates fresh import/image/sets,
        // regardless of same dimensions or reused numeric Vulkan driver handles.
        let texture = unsafe { Texture::new(gpu.ctx.clone(), &lease) }.map_err(err)?;
        let actual = draw_frame(gpu, &mut target, &mut renderer, &texture, Some(lease), true)?;
        let mut reference = vec![255u8; 16 * 8 * 4];
        for row in 0..8usize {
            reference[row * 64..row * 64 + width as usize * 4]
                .copy_from_slice(&pixels[row * width as usize * 4..(row + 1) * width as usize * 4]);
        }
        compare(&actual, &reference, false)?;
        drop(texture);
        drop(mapping);
        if gpu.ctx.counts.snapshot() != (0, 0, 0) || gpu.ctx.budget.used() != 0 {
            return Err(format!(
                "generation {generation}: retained GPU objects after drained fence"
            ));
        }
    }
    Ok(())
}

pub fn validation_errors() -> u64 {
    ERRORS.load(Ordering::SeqCst)
}
