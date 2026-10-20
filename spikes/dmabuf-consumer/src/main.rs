//! Standalone import check: connects to the Electron host like the layer would, takes the first
//! frame, prints its center pixel as `RRGGBBAA`, acks and exits.
//! - `Slot` + `FrameShm` (CPU-rendered widgets, Amendment A): reads the pixel from the slot.
//! - `Frame` (dmabuf): imports it, GPU-copies it into its own image and reads that back (a CPU
//!   readback is fine here: this is a test tool, not the frame path).
//!
//!   uw-spike-consumer [socket]   (default ~/.cache/underwolf-spike/overlay.sock)

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ash::vk;
use uw_spike_dmabuf::{DEVICE_EXTENSIONS, Dmabuf, Imported, acquire_barrier, find_memory_type, import, release_barrier};
use uw_spike_ipc::{DRM_FORMAT_ARGB8888, Msg, PROTOCOL, recv, send};

type Res<T> = Result<T, String>;

fn default_socket() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/underwolf-spike/overlay.sock")
}

fn connect_retry(path: &PathBuf, within: Duration) -> Res<UnixStream> {
    let start = Instant::now();
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(e) if start.elapsed() >= within => return Err(format!("connect {}: {e}", path.display())),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

struct Gpu {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    family: u32,
    queue: vk::Queue,
    fd_fns: ash::khr::external_memory_fd::Device,
    mem_props: vk::PhysicalDeviceMemoryProperties,
}

unsafe fn gpu() -> Res<Gpu> {
    unsafe {
        let entry = ash::Entry::load().map_err(|e| e.to_string())?;
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
        let instance = entry
            .create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)
            .map_err(|e| format!("vkCreateInstance: {e}"))?;
        for physical in instance.enumerate_physical_devices().map_err(|e| e.to_string())? {
            let available = instance.enumerate_device_extension_properties(physical).map_err(|e| e.to_string())?;
            let has = |name: &std::ffi::CStr| available.iter().any(|p| p.extension_name_as_c_str() == Ok(name));
            if !DEVICE_EXTENSIONS.iter().all(|e| has(e)) {
                continue;
            }
            let families = instance.get_physical_device_queue_family_properties(physical);
            let Some(family) = families.iter().position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS)) else {
                continue;
            };
            let family = family as u32;
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
            let exts: Vec<_> = DEVICE_EXTENSIONS.iter().map(|e| e.as_ptr()).collect();
            let device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&exts),
                    None,
                )
                .map_err(|e| format!("vkCreateDevice: {e}"))?;
            let queue = device.get_device_queue(family, 0);
            let fd_fns = ash::khr::external_memory_fd::Device::new(&instance, &device);
            let mem_props = instance.get_physical_device_memory_properties(physical);
            return Ok(Gpu { _entry: entry, instance, device, family, queue, fd_fns, mem_props });
        }
        Err("no Vulkan device supports the dmabuf import extensions".into())
    }
}

/// GPU-copies `src` into an image we own, then that into a host-visible buffer, and returns
/// the center pixel as RGBA bytes.
unsafe fn center_pixel(g: &Gpu, src: &Imported) -> Res<[u8; 4]> {
    let d = &g.device;
    let vkerr = |what: &'static str| move |e: vk::Result| format!("{what}: {e}");
    let (w, h) = (src.extent.width, src.extent.height);
    unsafe {
        let own = d
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(src.format)
                    .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .map_err(vkerr("create own image"))?;
        let reqs = d.get_image_memory_requirements(own);
        let own_mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(reqs.size).memory_type_index(
                    find_memory_type(&g.mem_props, reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
                        .ok_or("no device-local memory")?,
                ),
                None,
            )
            .map_err(vkerr("alloc own image"))?;
        d.bind_image_memory(own, own_mem, 0).map_err(vkerr("bind own image"))?;

        let size = w as u64 * h as u64 * 4;
        let buffer = d
            .create_buffer(
                &vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_DST),
                None,
            )
            .map_err(vkerr("create buffer"))?;
        let breqs = d.get_buffer_memory_requirements(buffer);
        let buf_mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(breqs.size).memory_type_index(
                    find_memory_type(
                        &g.mem_props,
                        breqs.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
                    .ok_or("no host-visible memory")?,
                ),
                None,
            )
            .map_err(vkerr("alloc buffer"))?;
        d.bind_buffer_memory(buffer, buf_mem, 0).map_err(vkerr("bind buffer"))?;

        let pool = d
            .create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(g.family), None)
            .map_err(vkerr("create pool"))?;
        let cmd = d
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1),
            )
            .map_err(vkerr("alloc cmd"))?[0];
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).map_err(vkerr("begin"))?;

        let color = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let own_to_dst = vk::ImageMemoryBarrier::default()
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(own)
            .subresource_range(color);
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[acquire_barrier(src.image, g.family), own_to_dst],
        );
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let extent = vk::Extent3D { width: w, height: h, depth: 1 };
        d.cmd_copy_image(
            cmd,
            src.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            own,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::ImageCopy {
                src_subresource: layers,
                src_offset: vk::Offset3D::default(),
                dst_subresource: layers,
                dst_offset: vk::Offset3D::default(),
                extent,
            }],
        );
        let own_to_src = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(own)
            .subresource_range(color);
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[own_to_src, release_barrier(src.image, g.family)],
        );
        d.cmd_copy_image_to_buffer(
            cmd,
            own,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer,
            &[vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: layers,
                image_offset: vk::Offset3D::default(),
                image_extent: extent,
            }],
        );
        d.end_command_buffer(cmd).map_err(vkerr("end"))?;
        let fence = d.create_fence(&vk::FenceCreateInfo::default(), None).map_err(vkerr("fence"))?;
        let cmds = [cmd];
        d.queue_submit(g.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], fence)
            .map_err(vkerr("submit"))?;
        d.wait_for_fences(&[fence], true, u64::MAX).map_err(vkerr("wait"))?;

        let ptr = d.map_memory(buf_mem, 0, size, vk::MemoryMapFlags::empty()).map_err(vkerr("map"))? as *const u8;
        let at = ((h / 2) as usize * w as usize + (w / 2) as usize) * 4;
        let p = std::slice::from_raw_parts(ptr.add(at), 4);
        let rgba = match src.format {
            vk::Format::B8G8R8A8_UNORM => [p[2], p[1], p[0], p[3]],
            _ => [p[0], p[1], p[2], p[3]],
        };
        d.unmap_memory(buf_mem);

        d.destroy_fence(fence, None);
        d.destroy_command_pool(pool, None);
        d.destroy_buffer(buffer, None);
        d.free_memory(buf_mem, None);
        d.destroy_image(own, None);
        d.free_memory(own_mem, None);
        Ok(rgba)
    }
}

fn run() -> Res<String> {
    let path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(default_socket);
    // Set up Vulkan first so the frame is copied well inside the host's 100 ms ack window.
    let g = unsafe { gpu()? };
    let stream = connect_retry(&path, Duration::from_secs(10))?;
    send(&stream, &Msg::Hello { protocol: PROTOCOL, pid: std::process::id() }, &[]).map_err(|e| e.to_string())?;
    // (file, width, height, stride, fourcc) per (widget, slot).
    let mut slots: HashMap<(u32, u32), (File, u32, u32, u32, u32)> = HashMap::new();
    let (id, width, height, drm_fourcc, modifier, planes, fds) = loop {
        match recv(&stream).map_err(|e| format!("recv: {e}"))? {
            (Msg::Slot { widget, slot, width, height, stride, drm_fourcc }, mut fds) if !fds.is_empty() => {
                slots.insert((widget, slot), (File::from(fds.remove(0)), width, height, stride, drm_fourcc));
            }
            (Msg::FrameShm { id, widget, slot, .. }, _) => {
                let (file, w, h, stride, fourcc) = slots.get(&(widget, slot)).ok_or("FrameShm for unknown slot")?;
                let mut p = [0u8; 4];
                let at = (h / 2) as u64 * *stride as u64 + (w / 2) as u64 * 4;
                file.read_exact_at(&mut p, at).map_err(|e| format!("read slot: {e}"))?;
                let rgba = if *fourcc == DRM_FORMAT_ARGB8888 { [p[2], p[1], p[0], p[3]] } else { p };
                eprintln!("shm frame {id}: widget {widget} slot {slot} {w}x{h} stride {stride}");
                send(&stream, &Msg::Ack { id }, &[]).map_err(|e| e.to_string())?;
                return Ok(rgba.iter().map(|b| format!("{b:02x}")).collect());
            }
            (Msg::Frame { id, width, height, drm_fourcc, modifier, planes, .. }, fds) => {
                break (id, width, height, drm_fourcc, modifier, planes, fds);
            }
            _ => continue,
        }
    };
    eprintln!(
        "frame {id}: {width}x{height} fourcc={drm_fourcc:#x} modifier={modifier:#x} planes={planes:?} fds={}",
        fds.len()
    );
    let rgba = unsafe {
        let imported = import(&g.device, &g.fd_fns, &g.mem_props, Dmabuf { width, height, drm_fourcc, modifier, planes: &planes, fds })
            .map_err(|e| format!("dmabuf import: {e}"))?;
        let px = center_pixel(&g, &imported);
        imported.destroy(&g.device);
        g.device.destroy_device(None);
        g.instance.destroy_instance(None);
        px?
    };
    send(&stream, &Msg::Ack { id }, &[]).map_err(|e| e.to_string())?;
    Ok(rgba.iter().map(|b| format!("{b:02x}")).collect())
}

fn main() -> ExitCode {
    match run() {
        Ok(hex) => {
            println!("{hex}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("uw-spike-consumer: {e}");
            ExitCode::FAILURE
        }
    }
}
