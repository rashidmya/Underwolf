//! Draws overlay quads onto a swapchain image right before present.
//! One `Overlay` per swapchain; per-image framebuffer, command buffer, fence and semaphore.

use std::ffi::c_void;

use ash::vk;

use crate::loader_abi::PfnSetDeviceLoaderData;

const VERT_SPV: &[u8] = include_bytes!("../shaders/quad.vert.spv");
const FRAG_SPV: &[u8] = include_bytes!("../shaders/quad.frag.spv");

pub enum QuadSource {
    /// Premultiplied RGBA.
    Solid([f32; 4]),
}

pub struct Quad {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub source: QuadSource,
}

/// Pixel rect → Vulkan NDC `[x0, y0, x1, y1]` (y points down in Vulkan clip space).
pub fn quad_ndc(x: i32, y: i32, w: u32, h: u32, extent: (u32, u32)) -> [f32; 4] {
    let (ew, eh) = (extent.0 as f32, extent.1 as f32);
    let (x0, y0) = (x as f32, y as f32);
    let (x1, y1) = (x0 + w as f32, y0 + h as f32);
    [2.0 * x0 / ew - 1.0, 2.0 * y0 / eh - 1.0, 2.0 * x1 / ew - 1.0, 2.0 * y1 / eh - 1.0]
}

#[repr(C)]
struct Push {
    rect: [f32; 4],
    color: [f32; 4],
}

struct Frame {
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    semaphore: vk::Semaphore,
}

pub struct Overlay {
    device: ash::Device,
    set_loader_data: PfnSetDeviceLoaderData,
    extent: vk::Extent2D,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    frames: Vec<Frame>,
    pool: Option<(u32, vk::CommandPool)>,
}

impl Overlay {
    /// # Safety
    /// `images` must belong to a live swapchain created on `device` with `format`/`extent`
    /// and `COLOR_ATTACHMENT` usage.
    pub unsafe fn new(
        device: ash::Device,
        set_loader_data: PfnSetDeviceLoaderData,
        format: vk::Format,
        extent: vk::Extent2D,
        images: &[vk::Image],
    ) -> Result<Self, vk::Result> {
        let mut o = Overlay {
            device,
            set_loader_data,
            extent,
            render_pass: vk::RenderPass::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            frames: Vec::new(),
            pool: None,
        };
        // On error, `Drop` releases whatever was created so far.
        unsafe {
            o.render_pass = o.create_render_pass(format)?;
            o.layout = o.create_layout()?;
            o.pipeline = o.create_pipeline()?;
            for &image in images {
                let frame = o.create_frame(image, format)?;
                o.frames.push(frame);
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
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
        let color_ref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let deps = [vk::SubpassDependency {
            src_subpass: vk::SUBPASS_EXTERNAL,
            dst_subpass: 0,
            src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            src_access_mask: vk::AccessFlags::empty(),
            dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dependency_flags: vk::DependencyFlags::empty(),
        }];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachment)
            .subpasses(&subpass)
            .dependencies(&deps);
        unsafe { self.device.create_render_pass(&info, None) }
    }

    unsafe fn create_layout(&self) -> Result<vk::PipelineLayout, vk::Result> {
        let ranges = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: size_of::<Push>() as u32,
        }];
        let info = vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&ranges);
        unsafe { self.device.create_pipeline_layout(&info, None) }
    }

    unsafe fn shader(&self, spv: &[u8]) -> Result<vk::ShaderModule, vk::Result> {
        let words: Vec<u32> = spv.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        unsafe { self.device.create_shader_module(&info, None) }
    }

    unsafe fn create_pipeline(&self) -> Result<vk::Pipeline, vk::Result> {
        unsafe {
            let vert = self.shader(VERT_SPV)?;
            let frag = match self.shader(FRAG_SPV) {
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
            let input_assembly =
                vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
            let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .line_width(1.0);
            let multisample =
                vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
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
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
            let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
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
            let result = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &info, None);
            self.device.destroy_shader_module(vert, None);
            self.device.destroy_shader_module(frag, None);
            result.map(|p| p[0]).map_err(|(_, e)| e)
        }
    }

    unsafe fn create_frame(&self, image: vk::Image, format: vk::Format) -> Result<Frame, vk::Result> {
        unsafe {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let view = self.device.create_image_view(&view_info, None)?;
            let attachments = [view];
            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(self.render_pass)
                .attachments(&attachments)
                .width(self.extent.width)
                .height(self.extent.height)
                .layers(1);
            let framebuffer = self.device.create_framebuffer(&fb_info, None)?;
            let fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)?;
            let semaphore = self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
            Ok(Frame { view, framebuffer, cmd: vk::CommandBuffer::null(), fence, semaphore })
        }
    }

    /// (Re)creates the command pool when the present queue's family changes.
    unsafe fn ensure_pool(&mut self, family: u32) -> Result<(), vk::Result> {
        if matches!(self.pool, Some((f, _)) if f == family) {
            return Ok(());
        }
        unsafe {
            if let Some((_, pool)) = self.pool.take() {
                let fences: Vec<_> = self.frames.iter().map(|f| f.fence).collect();
                self.device.wait_for_fences(&fences, true, u64::MAX)?;
                self.device.destroy_command_pool(pool, None);
            }
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
                (self.set_loader_data)(self.device.handle(), ash::vk::Handle::as_raw(cmd) as *mut c_void).result()?;
            }
        }
        Ok(())
    }

    /// Records and submits the overlay draw for `image_index`. The submission waits on
    /// `wait` and signals the returned semaphore, which the real present must wait on.
    ///
    /// # Safety
    /// `queue` must support graphics and belong to queue family `family` on this device.
    pub unsafe fn draw(
        &mut self,
        queue: vk::Queue,
        family: u32,
        image_index: u32,
        wait: &[vk::Semaphore],
        quads: &[Quad],
    ) -> Result<vk::Semaphore, vk::Result> {
        unsafe {
            self.ensure_pool(family)?;
            let frame = self.frames.get(image_index as usize).ok_or(vk::Result::ERROR_UNKNOWN)?;
            let d = &self.device;
            d.wait_for_fences(&[frame.fence], true, u64::MAX)?;
            d.reset_command_buffer(frame.cmd, vk::CommandBufferResetFlags::empty())?;
            d.begin_command_buffer(
                frame.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let area = vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.extent };
            d.cmd_begin_render_pass(
                frame.cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(frame.framebuffer)
                    .render_area(area),
                vk::SubpassContents::INLINE,
            );
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: self.extent.width as f32,
                height: self.extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            d.cmd_set_viewport(frame.cmd, 0, &[viewport]);
            d.cmd_set_scissor(frame.cmd, 0, &[area]);
            d.cmd_bind_pipeline(frame.cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            let extent = (self.extent.width, self.extent.height);
            for q in quads {
                let QuadSource::Solid(color) = q.source;
                let push = Push { rect: quad_ndc(q.x, q.y, q.w, q.h, extent), color };
                let bytes = std::slice::from_raw_parts((&push as *const Push).cast::<u8>(), size_of::<Push>());
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

            let stages = vec![vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT; wait.len()];
            let cmds = [frame.cmd];
            let signal = [frame.semaphore];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(wait)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&cmds)
                .signal_semaphores(&signal);
            d.reset_fences(&[frame.fence])?;
            d.queue_submit(queue, &[submit], frame.fence)?;
            Ok(frame.semaphore)
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let d = &self.device;
            let _ = d.device_wait_idle();
            for f in self.frames.drain(..) {
                d.destroy_semaphore(f.semaphore, None);
                d.destroy_fence(f.fence, None);
                d.destroy_framebuffer(f.framebuffer, None);
                d.destroy_image_view(f.view, None);
            }
            if let Some((_, pool)) = self.pool.take() {
                d.destroy_command_pool(pool, None);
            }
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_render_pass(self.render_pass, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_maps_pixels_to_ndc() {
        let r = quad_ndc(100, 100, 400, 300, (1920, 1080));
        let want = [-0.895_833_3, -0.814_814_8, -0.479_166_7, -0.259_259_3];
        for (a, b) in r.iter().zip(want) {
            assert!((a - b).abs() < 1e-5, "{r:?} vs {want:?}");
        }
        assert_eq!(quad_ndc(0, 0, 1920, 1080, (1920, 1080)), [-1.0, -1.0, 1.0, 1.0]);
    }
}
