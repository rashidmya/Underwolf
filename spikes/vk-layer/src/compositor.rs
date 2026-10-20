//! Per-device widget state: host shared-memory slots imported as `VkBuffer`s
//! (`VK_EXT_external_memory_host`, so the GPU reads the host's pages directly) and a
//! layer-owned image per widget that frames are copied into and sampled from.

use std::collections::HashMap;
use std::ffi::c_void;

use ash::vk;
use uw_spike_dmabuf::find_memory_type;

use crate::client::SlotMap;

pub fn is_srgb(f: vk::Format) -> bool {
    matches!(
        f,
        vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB | vk::Format::A8B8G8R8_SRGB_PACK32
    )
}

/// View format for widget images on a swapchain of format `swapchain`.
pub fn widget_view_format(swapchain: vk::Format) -> vk::Format {
    if is_srgb(swapchain) { vk::Format::B8G8R8A8_SRGB } else { vk::Format::B8G8R8A8_UNORM }
}

/// Host-pointer imports must cover whole multiples of `minImportedHostPointerAlignment`.
pub fn import_len(len: usize, align: u64) -> u64 {
    (len as u64).div_ceil(align) * align
}

/// Objects that may still be read by in-flight submissions. Each is tagged with the present
/// epoch it was retired in and freed once every submission up to that epoch has completed.
/// (The layer must never call vkDeviceWaitIdle from the game's present call: that requires
/// exclusive access to all of the game's queues, which other game threads may be using.)
pub struct Graveyard<T> {
    items: Vec<(u64, T)>,
}

impl<T> Default for Graveyard<T> {
    fn default() -> Self {
        Graveyard { items: Vec::new() }
    }
}

impl<T> Graveyard<T> {
    pub fn retire(&mut self, epoch: u64, item: T) {
        self.items.push((epoch, item));
    }

    /// Removes and returns every item whose epoch `safe` says has completed.
    pub fn sweep(&mut self, safe: impl Fn(u64) -> bool) -> Vec<T> {
        let (done, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.items).into_iter().partition(|(e, _)| safe(*e));
        self.items = keep;
        done.into_iter().map(|(_, t)| t).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

enum Garbage {
    Widget(WidgetImage),
    Import(Import),
}

struct Import {
    generation: u64,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

pub struct WidgetImage {
    /// Unique per image ever created (handles can be reused after destruction).
    pub serial: u64,
    pub image: vk::Image,
    memory: vk::DeviceMemory,
    pub view_unorm: vk::ImageView,
    pub view_srgb: vk::ImageView,
    pub width: u32,
    pub height: u32,
    /// False until the first upload has been recorded (its layout is still UNDEFINED).
    pub initialized: bool,
    pub x: i32,
    pub y: i32,
}

/// One buffer → image copy to record before drawing.
pub struct Upload {
    pub buffer: vk::Buffer,
    pub image: vk::Image,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub first: bool,
}

pub struct Compositor {
    device: ash::Device,
    host_fns: ash::ext::external_memory_host::DeviceFn,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    align: u64,
    pub set_layout: vk::DescriptorSetLayout,
    pub sampler: vk::Sampler,
    imports: HashMap<(u32, u32), Import>,
    pub widgets: HashMap<u32, WidgetImage>,
    /// `client::connection()` value the widgets belong to.
    pub connection: u64,
    /// Incremented once per present that draws; submissions are tagged with it.
    epoch: u64,
    graveyard: Graveyard<Garbage>,
    next_serial: u64,
}

impl Compositor {
    /// # Safety
    /// `device` must have `VK_EXT_external_memory_host` enabled; `host_fns` loaded for it.
    pub unsafe fn new(
        device: ash::Device,
        host_fns: ash::ext::external_memory_host::DeviceFn,
        mem_props: vk::PhysicalDeviceMemoryProperties,
        align: u64,
    ) -> Result<Compositor, vk::Result> {
        unsafe {
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            let set_layout = device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
            // Widgets are drawn 1:1, so nearest sampling keeps text sharp.
            let sampler = match device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    device.destroy_descriptor_set_layout(set_layout, None);
                    return Err(e);
                }
            };
            Ok(Compositor {
                device,
                host_fns,
                mem_props,
                align,
                set_layout,
                sampler,
                imports: HashMap::new(),
                widgets: HashMap::new(),
                connection: 0,
                epoch: 0,
                graveyard: Graveyard::default(),
                next_serial: 0,
            })
        }
    }

    /// The `VkBuffer` aliasing slot `key`'s host mapping, importing it on first use or after
    /// the host re-registered the slot.
    pub unsafe fn import(&mut self, key: (u32, u32), slot: &SlotMap) -> Result<vk::Buffer, vk::Result> {
        if let Some(i) = self.imports.get(&key)
            && i.generation == slot.generation
        {
            return Ok(i.buffer);
        }
        unsafe {
            let d = &self.device;
            if let Some(old) = self.imports.remove(&key) {
                self.graveyard.retire(self.epoch, Garbage::Import(old));
            }
            let size = import_len(slot.len, self.align);
            let handle = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;
            let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
            let buffer = d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .push_next(&mut ext),
                None,
            )?;
            let result = (|| {
                let mut props = vk::MemoryHostPointerPropertiesEXT::default();
                (self.host_fns.get_memory_host_pointer_properties_ext)(
                    d.handle(),
                    handle,
                    slot.addr as *const c_void,
                    &mut props,
                )
                .result()?;
                let reqs = d.get_buffer_memory_requirements(buffer);
                let type_index = find_memory_type(
                    &self.mem_props,
                    reqs.memory_type_bits & props.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
                .ok_or(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
                let mut import = vk::ImportMemoryHostPointerInfoEXT::default()
                    .handle_type(handle)
                    .host_pointer(slot.addr as *mut c_void);
                let memory = d.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(size)
                        .memory_type_index(type_index)
                        .push_next(&mut import),
                    None,
                )?;
                if let Err(e) = d.bind_buffer_memory(buffer, memory, 0) {
                    d.free_memory(memory, None);
                    return Err(e);
                }
                Ok(memory)
            })();
            match result {
                Ok(memory) => {
                    self.imports.insert(key, Import { generation: slot.generation, buffer, memory });
                    Ok(buffer)
                }
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    Err(e)
                }
            }
        }
    }

    /// Starts a present: returns the epoch its submissions are tagged with.
    pub fn begin_present(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// Retires all widgets and imports (the host disconnected or reconnected).
    pub fn clear(&mut self) {
        for (_, w) in std::mem::take(&mut self.widgets) {
            self.graveyard.retire(self.epoch, Garbage::Widget(w));
        }
        for (_, i) in std::mem::take(&mut self.imports) {
            self.graveyard.retire(self.epoch, Garbage::Import(i));
        }
    }

    /// Frees retired objects whose epoch `done` reports as completed on the GPU.
    pub unsafe fn sweep(&mut self, done: impl Fn(u64) -> bool) {
        for g in self.graveyard.sweep(done) {
            unsafe { self.destroy(g) };
        }
    }

    unsafe fn destroy(&self, g: Garbage) {
        unsafe {
            match g {
                Garbage::Widget(w) => self.destroy_widget(w),
                Garbage::Import(i) => {
                    self.device.destroy_buffer(i.buffer, None);
                    self.device.free_memory(i.memory, None);
                }
            }
        }
    }

    /// The widget's image, (re)created when its size changes.
    pub unsafe fn widget(&mut self, id: u32, width: u32, height: u32) -> Result<&mut WidgetImage, vk::Result> {
        let fits = self.widgets.get(&id).is_some_and(|w| w.width == width && w.height == height);
        if !fits {
            if let Some(old) = self.widgets.remove(&id) {
                self.graveyard.retire(self.epoch, Garbage::Widget(old));
            }
            let mut w = unsafe { self.create_widget(width, height)? };
            self.next_serial += 1;
            w.serial = self.next_serial;
            self.widgets.insert(id, w);
        }
        Ok(self.widgets.get_mut(&id).expect("present"))
    }

    unsafe fn create_widget(&self, width: u32, height: u32) -> Result<WidgetImage, vk::Result> {
        let d = &self.device;
        unsafe {
            // MUTABLE_FORMAT for the UNORM and SRGB views. (A VkImageFormatListCreateInfo hint
            // would need VK_KHR_image_format_list on 1.0/1.1 devices; it's optional.)
            let image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?;
            let mut w = WidgetImage {
                serial: 0,
                image,
                memory: vk::DeviceMemory::null(),
                view_unorm: vk::ImageView::null(),
                view_srgb: vk::ImageView::null(),
                width,
                height,
                initialized: false,
                x: 0,
                y: 0,
            };
            let result = (|| {
                let reqs = d.get_image_memory_requirements(image);
                let type_index =
                    find_memory_type(&self.mem_props, reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
                        .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
                w.memory = d.allocate_memory(
                    &vk::MemoryAllocateInfo::default().allocation_size(reqs.size).memory_type_index(type_index),
                    None,
                )?;
                d.bind_image_memory(image, w.memory, 0)?;
                let view = |format| {
                    d.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(format)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: 0,
                                layer_count: 1,
                            }),
                        None,
                    )
                };
                w.view_unorm = view(vk::Format::B8G8R8A8_UNORM)?;
                w.view_srgb = view(vk::Format::B8G8R8A8_SRGB)?;
                Ok(())
            })();
            match result {
                Ok(()) => Ok(w),
                Err(e) => {
                    self.destroy_widget(w);
                    Err(e)
                }
            }
        }
    }

    unsafe fn destroy_widget(&self, w: WidgetImage) {
        unsafe {
            let d = &self.device;
            d.destroy_image_view(w.view_srgb, None);
            d.destroy_image_view(w.view_unorm, None);
            d.destroy_image(w.image, None);
            d.free_memory(w.memory, None);
        }
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        unsafe {
            // Device destruction: the app has stopped using its queues, so a full wait is allowed.
            let _ = self.device.device_wait_idle();
            self.clear();
            for g in self.graveyard.sweep(|_| true) {
                self.destroy(g);
            }
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_view_matches_swapchain_encoding() {
        // Chromium's pixels are sRGB-encoded; an sRGB swapchain needs an sRGB view so the
        // hardware decodes on sample and re-encodes on write.
        assert_eq!(widget_view_format(vk::Format::B8G8R8A8_SRGB), vk::Format::B8G8R8A8_SRGB);
        assert_eq!(widget_view_format(vk::Format::R8G8B8A8_SRGB), vk::Format::B8G8R8A8_SRGB);
        assert_eq!(widget_view_format(vk::Format::A8B8G8R8_SRGB_PACK32), vk::Format::B8G8R8A8_SRGB);
        assert_eq!(widget_view_format(vk::Format::B8G8R8A8_UNORM), vk::Format::B8G8R8A8_UNORM);
        assert_eq!(widget_view_format(vk::Format::A2B10G10R10_UNORM_PACK32), vk::Format::B8G8R8A8_UNORM);
    }

    #[test]
    fn graveyard_frees_only_after_epoch_is_safe() {
        let mut g = Graveyard::default();
        g.retire(5, "a");
        g.retire(7, "b");
        assert!(g.sweep(|e| e < 5).is_empty());
        assert_eq!(g.sweep(|e| e <= 5), vec!["a"]);
        assert!(g.sweep(|e| e <= 6).is_empty());
        assert_eq!(g.sweep(|_| true), vec!["b"]);
        assert!(g.is_empty());
    }

    #[test]
    fn host_import_length_rounds_to_alignment() {
        assert_eq!(import_len(1600 * 300, 4096), 483_328);
        assert_eq!(import_len(4096, 4096), 4096);
        assert_eq!(import_len(1, 0x1000), 0x1000);
    }
}
