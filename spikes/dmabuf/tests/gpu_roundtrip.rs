//! Exports a LINEAR dmabuf filled with a known colour from Vulkan, imports it with
//! `uw_spike_dmabuf::import`, and reads it back. Proves the import path on this GPU
//! independently of Chromium. Skips (passes) when no capable Vulkan device exists.

use std::os::fd::{FromRawFd, OwnedFd};

use ash::vk;
use uw_spike_dmabuf::{DEVICE_EXTENSIONS, Dmabuf, acquire_barrier, find_memory_type, import, release_barrier};
use uw_spike_ipc::{DRM_FORMAT_ARGB8888, Plane};

const W: u32 = 64;
const H: u32 = 32;
const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};
const LAYERS: vk::ImageSubresourceLayers =
    vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };

struct Ctx {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    family: u32,
    queue: vk::Queue,
    fd_fns: ash::khr::external_memory_fd::Device,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    pool: vk::CommandPool,
}

unsafe fn ctx() -> Option<Ctx> {
    unsafe {
        let entry = ash::Entry::load().ok()?;
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
        let instance = entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None).ok()?;
        for physical in instance.enumerate_physical_devices().ok()? {
            let available = instance.enumerate_device_extension_properties(physical).ok()?;
            let has = |n: &std::ffi::CStr| available.iter().any(|p| p.extension_name_as_c_str() == Ok(n));
            if !DEVICE_EXTENSIONS.iter().all(|e| has(e)) {
                continue;
            }
            let family = instance
                .get_physical_device_queue_family_properties(physical)
                .iter()
                .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))? as u32;
            let prio = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let exts: Vec<_> = DEVICE_EXTENSIONS.iter().map(|e| e.as_ptr()).collect();
            let device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&exts),
                    None,
                )
                .ok()?;
            let queue = device.get_device_queue(family, 0);
            let fd_fns = ash::khr::external_memory_fd::Device::new(&instance, &device);
            let mem_props = instance.get_physical_device_memory_properties(physical);
            let pool = device
                .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family), None)
                .ok()?;
            return Some(Ctx { _entry: entry, instance, device, family, queue, fd_fns, mem_props, pool });
        }
        None
    }
}

unsafe fn submit(c: &Ctx, record: impl FnOnce(vk::CommandBuffer)) {
    unsafe {
        let d = &c.device;
        let cmd = d
            .allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(c.pool).command_buffer_count(1))
            .unwrap()[0];
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).unwrap();
        record(cmd);
        d.end_command_buffer(cmd).unwrap();
        let fence = d.create_fence(&vk::FenceCreateInfo::default(), None).unwrap();
        let cmds = [cmd];
        d.queue_submit(c.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], fence).unwrap();
        d.wait_for_fences(&[fence], true, u64::MAX).unwrap();
        d.destroy_fence(fence, None);
        d.free_command_buffers(c.pool, &cmds);
    }
}

fn barrier(image: vk::Image, old: vk::ImageLayout, new: vk::ImageLayout, src_q: u32, dst_q: u32) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(src_q)
        .dst_queue_family_index(dst_q)
        .image(image)
        .subresource_range(COLOR)
}

/// Creates a LINEAR-modifier dmabuf image, clears it to `bgra`, hands it to the foreign
/// queue family, and returns (fd, plane layout).
unsafe fn export_filled(c: &Ctx, bgra: [f32; 4]) -> (OwnedFd, Plane) {
    unsafe {
        let d = &c.device;
        let modifiers = [0u64]; // DRM_FORMAT_MOD_LINEAR
        let mut list = vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(&modifiers);
        let mut external =
            vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let image = d
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .extent(vk::Extent3D { width: W, height: H, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .push_next(&mut list)
                    .push_next(&mut external),
                None,
            )
            .unwrap();
        let reqs = d.get_image_memory_requirements(image);
        let mut export =
            vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let memory = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(reqs.size)
                    .memory_type_index(find_memory_type(&c.mem_props, reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL).unwrap())
                    .push_next(&mut export)
                    .push_next(&mut dedicated),
                None,
            )
            .unwrap();
        d.bind_image_memory(image, memory, 0).unwrap();
        let layout = d.get_image_subresource_layout(
            image,
            vk::ImageSubresource { aspect_mask: vk::ImageAspectFlags::MEMORY_PLANE_0_EXT, mip_level: 0, array_layer: 0 },
        );
        submit(c, |cmd| {
            let to_dst = barrier(image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED);
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &[to_dst]);
            let clear = vk::ClearColorValue { float32: bgra_to_rgba_unorm(bgra) };
            d.cmd_clear_color_image(cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear, &[COLOR]);
            let release = barrier(image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL, c.family, vk::QUEUE_FAMILY_FOREIGN_EXT);
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &[release]);
        });
        let raw = c
            .fd_fns
            .get_memory_fd(
                &vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
            )
            .unwrap();
        // The exported fd keeps the memory alive; the exporter's handles can go.
        d.destroy_image(image, None);
        d.free_memory(memory, None);
        (OwnedFd::from_raw_fd(raw), Plane { offset: layout.offset as u32, stride: layout.row_pitch as u32 })
    }
}

/// `ClearColorValue` is in the image format's component order (R, G, B, A) regardless of
/// memory order; the test speaks in BGRA bytes, so swizzle.
fn bgra_to_rgba_unorm(bgra: [f32; 4]) -> [f32; 4] {
    [bgra[2], bgra[1], bgra[0], bgra[3]]
}

unsafe fn read_center(c: &Ctx, image: vk::Image) -> [u8; 4] {
    unsafe {
        let d = &c.device;
        let size = (W * H * 4) as u64;
        let buffer = d
            .create_buffer(&vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_DST), None)
            .unwrap();
        let reqs = d.get_buffer_memory_requirements(buffer);
        let mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(reqs.size).memory_type_index(
                    find_memory_type(
                        &c.mem_props,
                        reqs.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
                    .unwrap(),
                ),
                None,
            )
            .unwrap();
        d.bind_buffer_memory(buffer, mem, 0).unwrap();
        submit(c, |cmd| {
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &[acquire_barrier(image, c.family)]);
            d.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: LAYERS,
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D { width: W, height: H, depth: 1 },
                }],
            );
            d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &[release_barrier(image, c.family)]);
        });
        let p = d.map_memory(mem, 0, size, vk::MemoryMapFlags::empty()).unwrap() as *const u8;
        let at = (((H / 2) * W + W / 2) * 4) as usize;
        let px = [*p.add(at), *p.add(at + 1), *p.add(at + 2), *p.add(at + 3)];
        d.unmap_memory(mem);
        d.destroy_buffer(buffer, None);
        d.free_memory(mem, None);
        px
    }
}

#[test]
fn exported_dmabuf_imports_with_same_pixels() {
    unsafe {
        let Some(c) = ctx() else {
            eprintln!("no Vulkan device with dmabuf import support; skipping");
            return;
        };
        // BGRA bytes 0x80, 0x00, 0xff, 0xff = rgb(255, 0, 128), opaque.
        let (fd, plane) = export_filled(&c, [128.0 / 255.0, 0.0, 1.0, 1.0]);
        let planes = [plane];
        let imported = import(
            &c.device,
            &c.fd_fns,
            &c.mem_props,
            Dmabuf { width: W, height: H, drm_fourcc: DRM_FORMAT_ARGB8888, modifier: 0, planes: &planes, fds: vec![fd] },
        )
        .expect("import");
        assert_eq!(imported.format, vk::Format::B8G8R8A8_UNORM);
        let px = read_center(&c, imported.image);
        imported.destroy(&c.device);
        c.device.destroy_command_pool(c.pool, None);
        c.device.destroy_device(None);
        c.instance.destroy_instance(None);
        assert_eq!(px, [0x80, 0x00, 0xff, 0xff]);
    }
}
