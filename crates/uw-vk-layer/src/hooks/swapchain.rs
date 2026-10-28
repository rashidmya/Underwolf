use crate::{
    dispatch::{self, key},
    draw::Renderer,
};
use ash::vk::{self, Handle};
pub unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    ci: *const vk::SwapchainCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    let Some(d) = dispatch::device(key(device)) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next) = d.create_swapchain else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    // SAFETY: valid original device/create/allocator/output pointers. Overlay
    // failures preserve the game's successful creation and original pNext nodes.
    unsafe {
        let Some(runtime) = &d.runtime else {
            return next(device, ci, alloc, out);
        };
        let format = (*ci).image_format;
        let supported_format = matches!(
            format,
            vk::Format::B8G8R8A8_UNORM
                | vk::Format::B8G8R8A8_SRGB
                | vk::Format::R8G8B8A8_UNORM
                | vk::Format::R8G8B8A8_SRGB
        );
        if !supported_format
            || (*ci).image_array_layers != 1
            || (*ci).flags.contains(vk::SwapchainCreateFlagsKHR::PROTECTED)
        {
            runtime.state.lock().unwrap().fail(2);
            return next(device, ci, alloc, out);
        }
        let mut patched = *ci;
        patched.image_usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
        let mut result = next(device, &patched, alloc, out);
        if result != vk::Result::SUCCESS && patched.image_usage != (*ci).image_usage {
            result = next(device, ci, alloc, out);
            if result == vk::Result::SUCCESS {
                runtime.state.lock().unwrap().fail(3);
            }
            return result;
        }
        if result != vk::Result::SUCCESS {
            return result;
        }
        let setup = (|| -> Result<(), vk::Result> {
            let get = d
                .get_swapchain_images
                .ok_or(vk::Result::ERROR_EXTENSION_NOT_PRESENT)?;
            let loader = d
                .set_loader_data
                .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
            let mut n = 0;
            get(device, *out, &mut n, std::ptr::null_mut()).result()?;
            if n == 0 || n > 8 {
                return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
            }
            let mut images = vec![vk::Image::null(); n as usize];
            get(device, *out, &mut n, images.as_mut_ptr()).result()?;
            images.truncate(n as usize);
            let mut state = runtime.state.lock().unwrap();
            if !state.available {
                return Ok(());
            }
            let count: usize = state.swaps.values().map(|r| r.frames.len()).sum::<usize>()
                + state
                    .old_swaps
                    .iter()
                    .map(|r| r.frames.len())
                    .sum::<usize>();
            if count + images.len() > 256 {
                return Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
            }
            state.next_swap = state
                .next_swap
                .checked_add(1)
                .ok_or(vk::Result::ERROR_TOO_MANY_OBJECTS)?;
            let generation = state.next_swap;
            let renderer = Renderer::new(
                state.ctx.clone(),
                loader,
                format,
                patched.image_extent,
                &images,
                generation,
            )?;
            if state.swaps.contains_key(&(*out).as_raw()) {
                return Err(vk::Result::ERROR_UNKNOWN);
            }
            state.swaps.insert((*out).as_raw(), renderer);
            state.notify(uw_frame_proto::Message::Swapchain {
                generation,
                width: patched.image_extent.width,
                height: patched.image_extent.height,
                ready: true,
            });
            Ok(())
        })();
        if setup.is_err() {
            runtime.state.lock().unwrap().fail(3);
        }
        result
    }
}
pub unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    let Some(d) = dispatch::device(key(device)) else {
        return;
    };
    let Some(next) = d.destroy_swapchain else {
        return;
    };
    if let Some(runtime) = &d.runtime {
        let mut state = runtime.state.lock().unwrap();
        if let Some(renderer) = state.swaps.remove(&swapchain.as_raw()) {
            let generation = renderer.generation;
            let (width, height) = (renderer.extent.width, renderer.extent.height);
            if generation == state.next_swap {
                state.notify(uw_frame_proto::Message::Swapchain {
                    generation,
                    width,
                    height,
                    ready: false,
                });
            }
            if let Err(renderer) = state.old_swaps.retire(vec![generation], renderer) {
                state.swaps.insert(swapchain.as_raw(), renderer);
                state.fail(7);
            }
        }
    }
    // SAFETY: forward exactly once with original allocator; layer objects keep
    // separate submission/presentation fence ownership until retirement.
    unsafe { next(device, swapchain, alloc) }
}
