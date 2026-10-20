//! Imports a single-plane dmabuf (from Electron's shared-texture frames) as a `VkImage`.
//! Used by the standalone consumer (Task 4) and the layer (Task 5).

use std::ffi::CStr;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use ash::vk;
use uw_spike_ipc::{DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, Plane};

/// Device extensions the import needs (on top of Vulkan 1.1 core).
pub const DEVICE_EXTENSIONS: &[&CStr] = &[
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
    ash::ext::image_drm_format_modifier::NAME,
    ash::ext::queue_family_foreign::NAME,
    ash::khr::image_format_list::NAME,
];

pub fn vk_format_for(drm_fourcc: u32) -> Option<vk::Format> {
    match drm_fourcc {
        DRM_FORMAT_ARGB8888 => Some(vk::Format::B8G8R8A8_UNORM),
        DRM_FORMAT_ABGR8888 => Some(vk::Format::R8G8B8A8_UNORM),
        _ => None,
    }
}

pub struct Dmabuf<'a> {
    pub width: u32,
    pub height: u32,
    pub drm_fourcc: u32,
    pub modifier: u64,
    pub planes: &'a [Plane],
    pub fds: Vec<OwnedFd>,
}

pub struct Imported {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
}

impl Imported {
    /// # Safety
    /// The GPU must no longer use the image.
    pub unsafe fn destroy(self, d: &ash::Device) {
        unsafe {
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
        }
    }
}

pub fn find_memory_type(props: &vk::PhysicalDeviceMemoryProperties, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
    (0..props.memory_type_count).find(|&i| {
        bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags)
    })
}

/// Imports `buf` as an image usable as a transfer source.
///
/// # Safety
/// `d` must have been created with [`DEVICE_EXTENSIONS`] enabled; `fd_fns` loaded for `d`.
pub unsafe fn import(
    d: &ash::Device,
    fd_fns: &ash::khr::external_memory_fd::Device,
    mem_props: &vk::PhysicalDeviceMemoryProperties,
    buf: Dmabuf<'_>,
) -> Result<Imported, vk::Result> {
    let format = vk_format_for(buf.drm_fourcc).ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?;
    // Electron's frames are single-plane; disjoint multi-plane import isn't needed here.
    let ([plane], [_]) = (buf.planes, &buf.fds[..]) else {
        return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
    };
    let fd = buf.fds.into_iter().next().ok_or(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
    let extent = vk::Extent2D { width: buf.width, height: buf.height };

    unsafe {
        let layouts = [vk::SubresourceLayout {
            offset: plane.offset as u64,
            size: 0,
            row_pitch: plane.stride as u64,
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(buf.modifier)
            .plane_layouts(&layouts);
        let mut external =
            vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: buf.width, height: buf.height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut modifier_info)
            .push_next(&mut external);
        let image = d.create_image(&info, None)?;

        let result = (|| {
            let mut fd_props = vk::MemoryFdPropertiesKHR::default();
            fd_fns.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                &mut fd_props,
            )?;
            let reqs = d.get_image_memory_requirements(image);
            let type_index = find_memory_type(
                mem_props,
                reqs.memory_type_bits & fd_props.memory_type_bits,
                vk::MemoryPropertyFlags::empty(),
            )
            .ok_or(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
            let raw = fd.into_raw_fd();
            let mut import_info = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(raw);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(reqs.size)
                .memory_type_index(type_index)
                .push_next(&mut import_info)
                .push_next(&mut dedicated);
            // On success the driver owns `raw`; on failure we must close it.
            let memory = d.allocate_memory(&alloc, None).inspect_err(|_| drop(OwnedFd::from_raw_fd(raw)))?;
            if let Err(e) = d.bind_image_memory(image, memory, 0) {
                d.free_memory(memory, None);
                return Err(e);
            }
            Ok(memory)
        })();
        match result {
            Ok(memory) => Ok(Imported { image, memory, format, extent }),
            Err(e) => {
                d.destroy_image(image, None);
                Err(e)
            }
        }
    }
}

const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

/// Takes ownership of an imported image from the foreign producer (Chromium), keeping its
/// contents, and transitions it for use as a transfer source.
pub fn acquire_barrier(image: vk::Image, family: u32) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
        .dst_queue_family_index(family)
        .image(image)
        .subresource_range(COLOR)
}

/// Hands the image back to the foreign producer after the copy.
pub fn release_barrier(image: vk::Image, family: u32) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
        .dst_access_mask(vk::AccessFlags::empty())
        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(family)
        .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
        .image(image)
        .subresource_range(COLOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uw_spike_ipc::{DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888};

    #[test]
    fn drm_fourcc_maps_to_vulkan_format() {
        assert_eq!(vk_format_for(DRM_FORMAT_ARGB8888), Some(vk::Format::B8G8R8A8_UNORM));
        assert_eq!(vk_format_for(DRM_FORMAT_ABGR8888), Some(vk::Format::R8G8B8A8_UNORM));
        assert_eq!(vk_format_for(0), None);
    }

    #[test]
    fn required_device_extensions() {
        let names: Vec<&str> = DEVICE_EXTENSIONS.iter().map(|c| c.to_str().unwrap()).collect();
        for e in [
            "VK_KHR_external_memory_fd",
            "VK_EXT_external_memory_dma_buf",
            "VK_EXT_image_drm_format_modifier",
            "VK_EXT_queue_family_foreign",
        ] {
            assert!(names.contains(&e), "missing {e}");
        }
    }
}
