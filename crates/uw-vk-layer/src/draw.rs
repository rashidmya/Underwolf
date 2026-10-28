//! Graphics pipeline mechanisms ported from the spike; no input/editing paths.
use crate::{
    DrawOutcome,
    lifetime::Frame,
    loader_abi::PfnSetDeviceLoaderData,
    resources::{COLOR_RANGE, Context, Texture, is_srgb},
    submit_if_ready, upload_region,
};
use ash::vk::{self, Handle};
use std::{ffi::c_void, sync::Arc};
use uw_frame_proto::Rect;
const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.frag.spv"));
const TEX_FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tex.frag.spv"));
#[repr(C)]
struct Push {
    rect: [f32; 4],
    color: [f32; 4],
}
pub struct Quad {
    pub rect: Rect,
    pub x: i32,
    pub y: i32,
    pub texture: Option<Arc<Texture>>,
    pub color: [f32; 4],
}
pub struct Upload<'a> {
    pub texture: &'a Texture,
    pub damage: Rect,
    pub first: bool,
}
pub struct Renderer {
    pub ctx: Arc<Context>,
    device: ash::Device,
    set_loader_data: PfnSetDeviceLoaderData,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    pub generation: u64,
    target_layout: vk::ImageLayout,
    render_pass: vk::RenderPass,
    set_layout: Option<vk::DescriptorSetLayout>,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    tex_pipeline: vk::Pipeline,
    pub frames: Vec<Frame>,
    pool: Option<(u32, vk::CommandPool)>,
}
impl Renderer {
    #[cfg(test)]
    pub(crate) fn testing(ctx: Arc<Context>) -> Self {
        unsafe extern "system" fn loader(_: vk::Device, _: *mut c_void) -> vk::Result {
            vk::Result::SUCCESS
        }
        Self {
            device: ctx.life.device.clone(),
            set_loader_data: loader,
            format: vk::Format::B8G8R8A8_UNORM,
            extent: vk::Extent2D {
                width: 4,
                height: 4,
            },
            generation: 1,
            target_layout: vk::ImageLayout::PRESENT_SRC_KHR,
            render_pass: vk::RenderPass::null(),
            set_layout: None,
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            tex_pipeline: vk::Pipeline::null(),
            frames: vec![Frame::new(ctx.clone())],
            pool: None,
            ctx,
        }
    }

    /// # Safety
    /// Images belong to a live compatible swapchain and support COLOR_ATTACHMENT;
    /// caller retains renderer until upload AND presentation fences complete.
    pub unsafe fn new(
        ctx: Arc<Context>,
        set_loader_data: PfnSetDeviceLoaderData,
        format: vk::Format,
        extent: vk::Extent2D,
        images: &[vk::Image],
        generation: u64,
    ) -> Result<Self, vk::Result> {
        // SAFETY: forward the caller's live swapchain-image contract.
        unsafe {
            Self::new_target(
                ctx,
                set_loader_data,
                format,
                extent,
                images,
                generation,
                vk::ImageLayout::PRESENT_SRC_KHR,
            )
        }
    }
    /// # Safety
    /// Offscreen image is in COLOR_ATTACHMENT_OPTIMAL and remains alive until
    /// every renderer submission completes; used by the owned GPU readback probe.
    #[allow(dead_code)]
    pub unsafe fn new_offscreen(
        ctx: Arc<Context>,
        loader: PfnSetDeviceLoaderData,
        format: vk::Format,
        extent: vk::Extent2D,
        image: vk::Image,
    ) -> Result<Self, vk::Result> {
        // SAFETY: forward the caller's retained offscreen target contract.
        unsafe {
            Self::new_target(
                ctx,
                loader,
                format,
                extent,
                &[image],
                1,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            )
        }
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn new_target(
        ctx: Arc<Context>,
        set_loader_data: PfnSetDeviceLoaderData,
        format: vk::Format,
        extent: vk::Extent2D,
        images: &[vk::Image],
        generation: u64,
        target_layout: vk::ImageLayout,
    ) -> Result<Self, vk::Result> {
        if images.is_empty() || images.len() > 8 || extent.width == 0 || extent.height == 0 {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        let mut o = Self {
            device: ctx.life.device.clone(),
            set_loader_data,
            format,
            extent,
            generation,
            target_layout,
            render_pass: vk::RenderPass::null(),
            set_layout: Some(ctx.layout),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            tex_pipeline: vk::Pipeline::null(),
            frames: vec![],
            pool: None,
            ctx,
        };
        // SAFETY: compatible live images; Drop releases partial construction.
        unsafe {
            o.render_pass = o.create_render_pass(format)?;
            o.layout = o.create_layout()?;
            o.pipeline = o.create_pipeline(FRAG_SPV)?;
            o.tex_pipeline = o.create_pipeline(TEX_FRAG_SPV)?;
            for &image in images {
                o.frames.push(o.create_frame(image, format)?);
            }
        }
        Ok(o)
    }
    unsafe fn create_render_pass(&self, format: vk::Format) -> Result<vk::RenderPass, vk::Result> {
        let attachment = [vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(self.target_layout)
            .final_layout(self.target_layout)];
        let color_ref = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let deps = [vk::SubpassDependency {
            src_subpass: vk::SUBPASS_EXTERNAL,
            dst_subpass: 0,
            src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            src_access_mask: vk::AccessFlags::empty(),
            dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_READ
                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dependency_flags: vk::DependencyFlags::empty(),
        }];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachment)
            .subpasses(&subpass)
            .dependencies(&deps);
        // SAFETY: live device and valid local attachment/subpass arrays.
        unsafe { self.device.create_render_pass(&info, None) }
    }

    unsafe fn create_layout(&self) -> Result<vk::PipelineLayout, vk::Result> {
        let ranges = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: size_of::<Push>() as u32,
        }];
        let set_layouts: Vec<_> = self.set_layout.into_iter().collect();
        let info = vk::PipelineLayoutCreateInfo::default()
            .push_constant_ranges(&ranges)
            .set_layouts(&set_layouts);
        // SAFETY: retained descriptor layout and valid local push ranges.
        unsafe { self.device.create_pipeline_layout(&info, None) }
    }

    unsafe fn shader(&self, spv: &[u8]) -> Result<vk::ShaderModule, vk::Result> {
        let words: Vec<u32> = spv
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: build-time compiled SPIR-V words remain live through the call.
        unsafe { self.device.create_shader_module(&info, None) }
    }

    unsafe fn create_pipeline(&self, frag_spv: &[u8]) -> Result<vk::Pipeline, vk::Result> {
        // SAFETY: local pipeline structures refer to retained render/layout and
        // live compiled shader modules, released after pipeline creation.
        unsafe {
            let vert = self.shader(VERT_SPV)?;
            let frag = match self.shader(frag_spv) {
                Ok(f) => f,
                Err(e) => {
                    self.device.destroy_shader_module(vert, None);
                    return Err(e);
                }
            };
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(vert)
                    .name(c"main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(frag)
                    .name(c"main"),
            ];
            let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
            let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
            let viewport = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            // Premultiplied alpha.
            let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend =
                vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic =
                vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let info = [vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex_input)
                .input_assembly_state(&input_assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multisample)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(self.layout)
                .render_pass(self.render_pass)
                .subpass(0)];
            let result =
                self.device
                    .create_graphics_pipelines(vk::PipelineCache::null(), &info, None);
            self.device.destroy_shader_module(vert, None);
            self.device.destroy_shader_module(frag, None);
            result.map(|p| p[0]).map_err(|(_, e)| e)
        }
    }

    unsafe fn create_frame(
        &self,
        image: vk::Image,
        format: vk::Format,
    ) -> Result<Frame, vk::Result> {
        let mut f = Frame::new(self.ctx.clone());
        // SAFETY: frame is not shared/submitted; partial failure invokes its Drop.
        unsafe {
            f.view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(COLOR_RANGE),
                None,
            )?;
            f.framebuffer = self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass)
                    .attachments(&[f.view])
                    .width(self.extent.width)
                    .height(self.extent.height)
                    .layers(1),
                None,
            )?;
            f.fence = self.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;
            f.present_fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)?;
            f.semaphore = self
                .device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
        }
        Ok(f)
    }
    unsafe fn ensure_pool(&mut self, family: u32) -> Result<(), vk::Result> {
        if let Some((old, _)) = self.pool {
            return if old == family {
                Ok(())
            } else {
                Err(vk::Result::ERROR_FEATURE_NOT_PRESENT)
            };
        }
        // SAFETY: no pool/commands exist yet. Loader callback initializes each
        // new command buffer's dispatch pointer before any use.
        unsafe {
            let pool = self.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(family),
                None,
            )?;
            self.pool = Some((family, pool));
            let cmds = self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(self.frames.len() as u32),
            )?;
            for (frame, cmd) in self.frames.iter_mut().zip(cmds) {
                frame.cmd = cmd;
                (self.set_loader_data)(self.device.handle(), cmd.as_raw() as *mut c_void)
                    .result()?;
            }
        }
        Ok(())
    }
    pub fn safe_to_retire(&self) -> bool {
        self.frames.iter().all(Frame::safe_to_retire)
    }
    /// # Safety
    /// Queue is the renderer's sole graphics/present queue; index was acquired
    /// by the game, original waits are live, and assets/leases are retained by
    /// the caller until moved to this frame following successful submission.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn draw(
        &mut self,
        queue: vk::Queue,
        family: u32,
        index: usize,
        wait: &[vk::Semaphore],
        quads: &[Quad],
        uploads: &[Upload<'_>],
        epoch: u64,
    ) -> DrawOutcome {
        if !self.frames.get(index).is_some_and(Frame::reusable) {
            return DrawOutcome::Skipped;
        }
        // SAFETY: exclusive render state; only completed command buffers/fences
        // are reset. Errors never manufacture a successful submission.
        let mut attempted_submit = false;
        // SAFETY: exclusive state and verified completed frame fences; local
        // arrays live through recording/submission and assets stay retained.
        let result = unsafe {
            (|| -> Result<(), vk::Result> {
                self.ensure_pool(family)?;
                let frame = &self.frames[index];
                let d = &self.device;
                d.reset_command_buffer(frame.cmd, vk::CommandBufferResetFlags::empty())?;
                d.begin_command_buffer(
                    frame.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )?;
                for upload in uploads {
                    record_upload(d, frame.cmd, upload)?;
                }
                let area = vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: self.extent,
                };
                d.cmd_begin_render_pass(
                    frame.cmd,
                    &vk::RenderPassBeginInfo::default()
                        .render_pass(self.render_pass)
                        .framebuffer(frame.framebuffer)
                        .render_area(area),
                    vk::SubpassContents::INLINE,
                );
                d.cmd_set_viewport(
                    frame.cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: self.extent.width as f32,
                        height: self.extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                d.cmd_set_scissor(frame.cmd, 0, &[area]);
                for q in quads {
                    if let Some(texture) = &q.texture {
                        d.cmd_bind_pipeline(
                            frame.cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.tex_pipeline,
                        );
                        d.cmd_bind_descriptor_sets(
                            frame.cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.layout,
                            0,
                            &[texture.sets[usize::from(is_srgb(self.format))]],
                            &[],
                        );
                    } else {
                        d.cmd_bind_pipeline(
                            frame.cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.pipeline,
                        );
                    }
                    let ew = self.extent.width as f32;
                    let eh = self.extent.height as f32;
                    let push = Push {
                        rect: [
                            2.0 * q.x as f32 / ew - 1.0,
                            2.0 * q.y as f32 / eh - 1.0,
                            2.0 * (q.x as f32 + q.rect.width as f32) / ew - 1.0,
                            2.0 * (q.y as f32 + q.rect.height as f32) / eh - 1.0,
                        ],
                        color: q.color,
                    };
                    let bytes = std::slice::from_raw_parts(
                        (&push as *const Push).cast::<u8>(),
                        size_of::<Push>(),
                    );
                    d.cmd_push_constants(
                        frame.cmd,
                        self.layout,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        bytes,
                    );
                    d.cmd_draw(frame.cmd, 4, 1, 0, 0);
                }
                d.cmd_end_render_pass(frame.cmd);
                d.end_command_buffer(frame.cmd)?;
                let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; wait.len()];
                let cmds = [frame.cmd];
                let signal = [frame.semaphore];
                let submit = vk::SubmitInfo::default()
                    .wait_semaphores(wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds)
                    .signal_semaphores(&signal);
                d.reset_fences(&[frame.fence, frame.present_fence])?;
                attempted_submit = true;
                d.queue_submit(queue, &[submit], frame.fence)?;
                Ok(())
            })()
        };
        let outcome = submit_if_ready(true, true, epoch, || result.is_ok());
        let frame = &mut self.frames[index];
        match outcome {
            DrawOutcome::Submitted { .. } => {
                frame.submitted = true;
                frame.draw_reported = quads.is_empty();
                frame.epoch = epoch;
            }
            _ => {
                frame.failed = true;
                if attempted_submit {
                    frame.submitted = true;
                    frame.draw_reported = true;
                }
            }
        };
        outcome
    }
}
/// # Safety
/// Command buffer is recording; the texture is retained, its upload lease owns
/// the host bytes, and all texture use is serialized on the sole supported queue.
pub unsafe fn record_upload(
    d: &ash::Device,
    cmd: vk::CommandBuffer,
    u: &Upload<'_>,
) -> Result<(), vk::Result> {
    let region = upload_region(u.texture.mapping.header().layout, u.damage)
        .map_err(|_| vk::Result::ERROR_UNKNOWN)?;
    let image_barrier = |old, new, src, dst| {
        vk::ImageMemoryBarrier::default()
            .src_access_mask(src)
            .dst_access_mask(dst)
            .old_layout(old)
            .new_layout(new)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(u.texture.image)
            .subresource_range(COLOR_RANGE)
    };
    // SAFETY: validated bounds/row pitch; barriers order prior sampling, coherent
    // host writes, transfer writes and subsequent sampling without discarding data.
    unsafe {
        let host = vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::HOST_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(u.texture.buffer)
            .offset(0)
            .size(vk::WHOLE_SIZE);
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[host],
            &[image_barrier(
                if u.first {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                },
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                if u.first {
                    vk::AccessFlags::empty()
                } else {
                    vk::AccessFlags::SHADER_READ
                },
                vk::AccessFlags::TRANSFER_WRITE,
            )],
        );
        let copy = vk::BufferImageCopy::default()
            .buffer_offset(region.offset)
            .buffer_row_length(region.row_length)
            .buffer_image_height(0)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D {
                x: u.damage.x as i32,
                y: u.damage.y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: u.damage.width,
                height: u.damage.height,
                depth: 1,
            });
        d.cmd_copy_buffer_to_image(
            cmd,
            u.texture.buffer,
            u.texture.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[copy],
        );
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[image_barrier(
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
            )],
        );
    }
    Ok(())
}
impl Drop for Renderer {
    fn drop(&mut self) {
        if !self.ctx.life.is_live() {
            return;
        }
        // SAFETY: retire queue requires all render AND present fences; partial
        // initialization has never submitted. No idle wait or timeout is substituted.
        unsafe {
            self.frames.clear();
            if let Some((_, pool)) = self.pool.take() {
                self.device.destroy_command_pool(pool, None);
            }
            self.device.destroy_pipeline(self.tex_pipeline, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device.destroy_render_pass(self.render_pass, None);
        }
    }
}
