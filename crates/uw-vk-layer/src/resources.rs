//! GPU objects own their imported mappings. Drops call the driver only while the
//! device is live, and callers retain every object through all submission fences.
use ash::vk;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uw_frame_channel::{Budget, BudgetReservation, FrameMapping, ReadLease};
pub const COLOR_RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};
pub struct DeviceLife {
    pub device: ash::Device,
    pub live: AtomicBool,
}
impl DeviceLife {
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}
pub struct Context {
    pub life: Arc<DeviceLife>,
    pub host: ash::ext::external_memory_host::DeviceFn,
    pub props: vk::PhysicalDeviceMemoryProperties,
    pub alignment: u64,
    pub layout: vk::DescriptorSetLayout,
    pub sampler: vk::Sampler,
    pub pool: vk::DescriptorPool,
    pub budget: Budget,
}
pub fn memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..props.memory_type_count).find(|&i| {
        bits & (1 << i) != 0
            && props.memory_types[i as usize]
                .property_flags
                .contains(flags)
    })
}
pub fn is_srgb(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB | vk::Format::A8B8G8R8_SRGB_PACK32
    )
}
impl Context {
    /// # Safety
    /// The device is live, has host import/maintenance enabled and all entrypoints
    /// refer to its next-layer dispatch. Its destruction is ordered by DeviceLife.
    pub unsafe fn new(
        device: ash::Device,
        host: ash::ext::external_memory_host::DeviceFn,
        props: vk::PhysicalDeviceMemoryProperties,
        alignment: u64,
    ) -> Result<Arc<Self>, vk::Result> {
        if !(4096..=1 << 20).contains(&alignment) || !alignment.is_power_of_two() {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        let mut ctx = Self {
            life: Arc::new(DeviceLife {
                device,
                live: AtomicBool::new(true),
            }),
            host,
            props,
            alignment,
            layout: vk::DescriptorSetLayout::null(),
            sampler: vk::Sampler::null(),
            pool: vk::DescriptorPool::null(),
            budget: Budget::new(512 * 1024 * 1024),
        };
        // SAFETY: required features/entrypoints were established by the caller;
        // partial construction is released by Drop without outstanding GPU use.
        unsafe {
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            ctx.layout = ctx.life.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            ctx.sampler = ctx.life.device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )?;
            let sizes = [vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 512,
            }];
            ctx.pool = ctx.life.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(512)
                    .pool_sizes(&sizes),
                None,
            )?;
        }
        Ok(Arc::new(ctx))
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        if !self.life.is_live() {
            return;
        }
        // SAFETY: textures/renderers retain Context, so the last owner has no GPU
        // references or is still in an unsubmitted failed construction.
        unsafe {
            let d = &self.life.device;
            d.destroy_descriptor_pool(self.pool, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}
pub struct Texture {
    pub ctx: Arc<Context>,
    pub mapping: Arc<FrameMapping>,
    pub image: vk::Image,
    pub buffer: vk::Buffer,
    image_memory: vk::DeviceMemory,
    import_memory: vk::DeviceMemory,
    views: [vk::ImageView; 2],
    pub sets: [vk::DescriptorSet; 2],
    _budget: Option<BudgetReservation>,
}
impl Texture {
    /// # Safety
    /// Caller owns the claimed pixels and retains this texture through all GPU
    /// fences referencing it. All creation/drop/descriptor operations serialize
    /// on this device's render mutex (including other textures).
    pub unsafe fn new(ctx: Arc<Context>, lease: &ReadLease) -> Result<Arc<Self>, vk::Result> {
        let layout = lease.mapping().header().layout;
        let ptr = lease.host_pointer();
        if !(ptr as usize as u64).is_multiple_of(ctx.alignment)
            || !layout.pixel_len.is_multiple_of(ctx.alignment)
        {
            return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
        }
        let mut t = Self {
            ctx,
            mapping: lease.mapping().clone(),
            image: vk::Image::null(),
            buffer: vk::Buffer::null(),
            image_memory: vk::DeviceMemory::null(),
            import_memory: vk::DeviceMemory::null(),
            views: [vk::ImageView::null(); 2],
            sets: [vk::DescriptorSet::null(); 2],
            _budget: None,
        };
        // SAFETY: the ReadLease pins all pixel bytes and excludes host writes;
        // each extent, memory requirement and type is checked before binding.
        unsafe {
            let d = &t.ctx.life.device;
            let handle = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;
            let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
            t.buffer = d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(layout.pixel_len)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .push_next(&mut external),
                None,
            )?;
            let req = d.get_buffer_memory_requirements(t.buffer);
            if req.size > layout.pixel_len {
                return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
            }
            let mut host_props = vk::MemoryHostPointerPropertiesEXT::default();
            (t.ctx.host.get_memory_host_pointer_properties_ext)(
                d.handle(),
                handle,
                ptr,
                &mut host_props,
            )
            .result()?;
            let host_type = memory_type(
                &t.ctx.props,
                req.memory_type_bits & host_props.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
            t.image = d.create_image(
                &vk::ImageCreateInfo::default()
                    .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .extent(vk::Extent3D {
                        width: layout.width,
                        height: layout.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?;
            let image_req = d.get_image_memory_requirements(t.image);
            let total = layout
                .pixel_len
                .checked_add(image_req.size)
                .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
            t._budget = Some(
                t.ctx
                    .budget
                    .reserve(total)
                    .map_err(|_| vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?,
            );
            let mut import = vk::ImportMemoryHostPointerInfoEXT::default()
                .handle_type(handle)
                .host_pointer(ptr);
            t.import_memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(layout.pixel_len)
                    .memory_type_index(host_type)
                    .push_next(&mut import),
                None,
            )?;
            d.bind_buffer_memory(t.buffer, t.import_memory, 0)?;
            let image_type = memory_type(
                &t.ctx.props,
                image_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
            t.image_memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(image_req.size)
                    .memory_type_index(image_type),
                None,
            )?;
            d.bind_image_memory(t.image, t.image_memory, 0)?;
            for (i, format) in [vk::Format::B8G8R8A8_UNORM, vk::Format::B8G8R8A8_SRGB]
                .into_iter()
                .enumerate()
            {
                t.views[i] = d.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(t.image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(COLOR_RANGE),
                    None,
                )?;
                let layouts = [t.ctx.layout];
                t.sets[i] = d.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(t.ctx.pool)
                        .set_layouts(&layouts),
                )?[0];
                let info = [vk::DescriptorImageInfo {
                    sampler: t.ctx.sampler,
                    image_view: t.views[i],
                    image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                }];
                d.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::default()
                        .dst_set(t.sets[i])
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&info)],
                    &[],
                );
            }
        }
        Ok(Arc::new(t))
    }
}
impl Drop for Texture {
    fn drop(&mut self) {
        if !self.ctx.life.is_live() {
            return;
        }
        // SAFETY: retirement holds the last Arc until every referencing submission
        // completes. A failed constructor has never been submitted.
        unsafe {
            let d = &self.ctx.life.device;
            let sets: Vec<_> = self
                .sets
                .iter()
                .copied()
                .filter(|s| *s != vk::DescriptorSet::null())
                .collect();
            if !sets.is_empty() {
                let _ = d.free_descriptor_sets(self.ctx.pool, &sets);
            }
            for view in self.views {
                d.destroy_image_view(view, None);
            }
            d.destroy_image(self.image, None);
            d.free_memory(self.image_memory, None);
            d.destroy_buffer(self.buffer, None);
            d.free_memory(self.import_memory, None);
        }
    }
}
