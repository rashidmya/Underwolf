use crate::{
    DrawOutcome,
    dispatch::{self, key},
};
use ash::vk;
pub unsafe extern "system" fn queue_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    let Some(d) = dispatch::device(key(queue)) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next) = d.queue_present else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    let mut sample = d.trace.as_ref().map(|trace| trace.begin());
    // SAFETY: valid live game queue and present info. Original arrays, results,
    // pNext and return value remain untouched on every skipped/unsupported path.
    unsafe {
        let Some(runtime) = &d.runtime else {
            return next(queue, info);
        };
        let Ok(mut state) = runtime.state.try_lock() else {
            return next(queue, info);
        };
        if (*info).swapchain_count != 1
            || !super::chain_find(
                (*info).p_next,
                vk::StructureType::SWAPCHAIN_PRESENT_FENCE_INFO_EXT,
            )
            .is_null()
        {
            state.fail(2);
            drop(state);
            return next(queue, info);
        }
        let family = match d.queue_family(queue) {
            Err(()) => {
                drop(state);
                return next(queue, info);
            }
            Ok(Some(family)) if d.family_supports_graphics(family) => family,
            _ => {
                state.fail(2);
                drop(state);
                return next(queue, info);
            }
        };
        let wait = if (*info).wait_semaphore_count == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(
                (*info).p_wait_semaphores,
                (*info).wait_semaphore_count as usize,
            )
        };
        let swapchain = *(*info).p_swapchains;
        let index = *(*info).p_image_indices as usize;
        let outcome = state.draw(swapchain, index, queue, family, wait);
        if !matches!(outcome, DrawOutcome::Submitted { .. }) {
            drop(state);
            return next(queue, info);
        }
        if let Some(sample) = &mut sample {
            sample.submitted = true;
        }
        let frame = &mut state
            .swaps
            .get_mut(&ash::vk::Handle::as_raw(swapchain))
            .unwrap()
            .frames[index];
        let signal = frame.semaphore;
        let fence = frame.present_fence;
        frame.present_pending = true;
        let present_active = frame.present_active.clone();
        present_active.store(true, std::sync::atomic::Ordering::Release);
        let generation = state.swaps[&ash::vk::Handle::as_raw(swapchain)].generation;
        drop(state);
        let result = forward_submitted_present(next, queue, info, signal, fence);
        present_active.store(false, std::sync::atomic::Ordering::Release);
        if result != vk::Result::SUCCESS
            && result != vk::Result::SUBOPTIMAL_KHR
            && let Ok(mut state) = runtime.state.try_lock()
        {
            if result == vk::Result::ERROR_OUT_OF_DATE_KHR {
                let extent = state
                    .swaps
                    .get(&ash::vk::Handle::as_raw(swapchain))
                    .map(|r| r.extent);
                if let Some(extent) = extent {
                    state.notify(uw_frame_proto::Message::Swapchain {
                        generation,
                        width: extent.width,
                        height: extent.height,
                        ready: false,
                    });
                }
            } else {
                state.fail(3);
            }
        }
        result
    }
}
/// # Safety
/// Caller has successfully submitted an overlay signal, owns an unsignaled
/// present fence, and verified no game-owned present-fence node exists.
unsafe fn forward_submitted_present(
    next: vk::PFN_vkQueuePresentKHR,
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
    signal: vk::Semaphore,
    fence: vk::Fence,
) -> vk::Result {
    // SAFETY: one swapchain and valid original present structure; the added
    // immutable node and arrays remain live through the downstream call.
    unsafe {
        let signals = [signal];
        let fences = [fence];
        let mut node = vk::SwapchainPresentFenceInfoEXT::default().fences(&fences);
        node.p_next = (*info).p_next;
        let mut patched = *info;
        patched.wait_semaphore_count = 1;
        patched.p_wait_semaphores = signals.as_ptr();
        patched.p_next = (&node as *const vk::SwapchainPresentFenceInfoEXT<'_>).cast();
        next(queue, &patched)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client::Runtime,
        dispatch::DeviceData,
        resources::{Context, DeviceLife},
    };
    use ash::vk::Handle;
    use std::{
        cell::RefCell,
        collections::HashMap,
        sync::{Arc, Mutex, atomic::AtomicBool},
    };
    #[derive(Default)]
    struct Observed {
        info: usize,
        waits: Vec<u64>,
        swaps: usize,
        indices: usize,
        results: usize,
        chain: usize,
        fence: u64,
    }
    thread_local! {static SEEN:RefCell<Observed>=RefCell::new(Observed::default());}
    unsafe extern "system" fn present(
        _: vk::Queue,
        p: *const vk::PresentInfoKHR<'_>,
    ) -> vk::Result {
        // SAFETY: layer hook/mock helper passes valid local arrays and pNext.
        unsafe {
            let mut result = Observed {
                info: p as usize,
                waits: if (*p).wait_semaphore_count == 0 {
                    vec![]
                } else {
                    std::slice::from_raw_parts(
                        (*p).p_wait_semaphores,
                        (*p).wait_semaphore_count as usize,
                    )
                    .iter()
                    .map(|s| s.as_raw())
                    .collect()
                },
                swaps: (*p).p_swapchains as usize,
                indices: (*p).p_image_indices as usize,
                results: (*p).p_results as usize,
                chain: (*p).p_next as usize,
                fence: 0,
            };
            let fence = super::super::chain_find(
                (*p).p_next,
                vk::StructureType::SWAPCHAIN_PRESENT_FENCE_INFO_EXT,
            )
            .cast::<vk::SwapchainPresentFenceInfoEXT<'_>>();
            if !fence.is_null() {
                result.fence = (*(*fence).p_fences).as_raw();
                result.chain = (*fence).p_next as usize;
            }
            if !(*p).p_results.is_null() {
                *(*p).p_results = vk::Result::ERROR_OUT_OF_DATE_KHR;
            }
            SEEN.with(|s| *s.borrow_mut() = result);
        }
        vk::Result::SUBOPTIMAL_KHR
    }
    unsafe extern "system" fn gdpa(
        _: vk::Device,
        _: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        None
    }
    unsafe extern "system" fn destroy(_: vk::Device, _: *const vk::AllocationCallbacks<'_>) {}
    unsafe extern "system" fn queue(_: vk::Device, _: u32, _: u32, _: *mut vk::Queue) {}
    fn context() -> Arc<Context> {
        // SAFETY: no driver calls in these passthrough tests; dead DeviceLife
        // suppresses all drops. Uncalled missing function stubs are sufficient.
        let ash = unsafe { dispatch::ash_device(gdpa, vk::Device::null()) };
        Arc::new(Context {
            handles_live: AtomicBool::new(false),
            counts: crate::resources::ResourceCounts::default(),
            life: Arc::new(DeviceLife {
                device: ash,
                live: AtomicBool::new(false),
            }),
            host: ash::ext::external_memory_host::DeviceFn::load(|_| std::ptr::null()),
            props: vk::PhysicalDeviceMemoryProperties::default(),
            alignment: 4096,
            layout: vk::DescriptorSetLayout::null(),
            sampler: vk::Sampler::null(),
            pool: vk::DescriptorPool::null(),
            budget: uw_frame_channel::Budget::new(1),
        })
    }
    fn fixture(runtime: Option<Runtime>, graphics: bool) -> (Box<usize>, vk::Queue) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0x991123);
        let word = Box::new(NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let queue = vk::Queue::from_raw((&*word as *const usize) as u64);
        let mut families = HashMap::new();
        families.insert(queue.as_raw(), 0);
        dispatch::insert_device(DeviceData {
            device: vk::Device::from_raw(queue.as_raw()),
            set_loader_data: None,
            queue_family_props: vec![vk::QueueFamilyProperties {
                queue_flags: if graphics {
                    vk::QueueFlags::GRAPHICS
                } else {
                    vk::QueueFlags::COMPUTE
                },
                ..Default::default()
            }],
            gdpa,
            destroy_device: destroy,
            get_device_queue: queue_fn(),
            get_device_queue2: None,
            create_swapchain: None,
            destroy_swapchain: None,
            get_swapchain_images: None,
            queue_present: Some(present),
            queue_families: Mutex::new(families),
            runtime,
            trace: None,
        });
        (word, queue)
    }
    fn queue_fn() -> vk::PFN_vkGetDeviceQueue {
        queue
    }
    #[test]
    fn non_target_and_busy_lock_forward_the_original_present_without_submission() {
        for busy in [false, true] {
            let runtime = busy.then(|| Runtime::testing(context()));
            let (_word, q) = fixture(runtime, true);
            let device = dispatch::device(key(q)).unwrap();
            let guard = device.runtime.as_ref().map(|r| r.state.lock().unwrap());
            let waits = [vk::Semaphore::from_raw(11), vk::Semaphore::from_raw(22)];
            let swaps = [vk::SwapchainKHR::from_raw(9)];
            let indices = [0];
            let info = vk::PresentInfoKHR::default()
                .wait_semaphores(&waits)
                .swapchains(&swaps)
                .image_indices(&indices);
            // SAFETY: mock queue has a live dispatch word and valid local arrays.
            let result = unsafe { queue_present(q, &info) };
            assert_eq!(result, vk::Result::SUBOPTIMAL_KHR);
            SEEN.with(|s| {
                let s = s.borrow();
                assert_eq!(s.info, (&info as *const vk::PresentInfoKHR<'_>) as usize);
                assert_eq!(s.waits, [11, 22]);
            });
            drop(guard);
            dispatch::remove_device(key(q));
        }
    }
    #[test]
    fn busy_queue_map_skips_without_making_the_renderer_unavailable() {
        let (_word, q) = fixture(Some(Runtime::testing(context())), true);
        let d = dispatch::device(key(q)).unwrap();
        let held = d.queue_families.lock().unwrap();
        let waits = [vk::Semaphore::from_raw(11)];
        let swaps = [vk::SwapchainKHR::from_raw(9)];
        let indices = [0];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swaps)
            .image_indices(&indices);
        // SAFETY: valid mock dispatch word and live local present arrays.
        let result = unsafe { queue_present(q, &info) };
        assert_eq!(result, vk::Result::SUBOPTIMAL_KHR);
        SEEN.with(|s| assert_eq!(s.borrow().waits, [11]));
        assert!(d.runtime.as_ref().unwrap().state.lock().unwrap().available);
        drop(held);
        dispatch::remove_device(key(q));
    }
    #[test]
    fn completed_upload_hints_coalesce_per_allocation_without_evicting_controls() {
        use uw_frame_proto::{AllocationKey, Message, SessionId, WidgetKey};
        let mut state = crate::compositor::State::new(context());
        let key = AllocationKey {
            session: SessionId([1; 16]),
            widget: WidgetKey {
                id: 1,
                generation: 1,
            },
            generation: 1,
        };
        state.notify(Message::Swapchain {
            generation: 1,
            width: 320,
            height: 240,
            ready: true,
        });
        for consumed in 1..1000 {
            state.notify(Message::Consumed { key, consumed });
        }
        state.notify(Message::Consumed { key, consumed: 3 });
        assert!(state.available);
        assert_eq!(state.outgoing.len(), 2);
        assert!(matches!(
            state.outgoing.front(),
            Some(Message::Swapchain { .. })
        ));
        assert!(matches!(
            state.outgoing.back(),
            Some(Message::Consumed { consumed: 999, .. })
        ));
    }
    #[test]
    fn unsignaled_frame_and_unsupported_queue_preserve_waits() {
        for graphics in [true, false] {
            let ctx = context();
            let runtime = Runtime::testing(ctx.clone());
            {
                let mut state = runtime.state.lock().unwrap();
                state.session = Some(uw_frame_proto::SessionId([1; 16]));
                let mut renderer = crate::draw::Renderer::testing(ctx);
                renderer.frames[0].submitted = true;
                state.swaps.insert(9, renderer);
            }
            let (_word, q) = fixture(Some(runtime), graphics);
            let waits = [vk::Semaphore::from_raw(11)];
            let swaps = [vk::SwapchainKHR::from_raw(9)];
            let indices = [0];
            let info = vk::PresentInfoKHR::default()
                .wait_semaphores(&waits)
                .swapchains(&swaps)
                .image_indices(&indices);
            // SAFETY: valid mock dispatch and retained inert renderer.
            let _ = unsafe { queue_present(q, &info) };
            SEEN.with(|s| assert_eq!(s.borrow().waits, [11]));
            let d = dispatch::remove_device(key(q)).unwrap();
            assert_eq!(
                d.runtime.as_ref().unwrap().state.lock().unwrap().available,
                graphics
            );
        }
    }
    #[test]
    fn submitted_present_chains_signal_and_own_fence_without_losing_game_fields() {
        let waits = [vk::Semaphore::from_raw(11)];
        let swaps = [vk::SwapchainKHR::from_raw(9)];
        let indices = [3];
        let mut results = [vk::Result::SUCCESS];
        let mut existing = vk::PresentIdKHR::default();
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swaps)
            .image_indices(&indices)
            .results(&mut results)
            .push_next(&mut existing);
        // SAFETY: local mock arrays/fence/semaphore remain valid for forwarding.
        let result = unsafe {
            forward_submitted_present(
                present,
                vk::Queue::null(),
                &info,
                vk::Semaphore::from_raw(77),
                vk::Fence::from_raw(88),
            )
        };
        assert_eq!(result, vk::Result::SUBOPTIMAL_KHR);
        SEEN.with(|s| {
            let s = s.borrow();
            assert_eq!(s.waits, [77]);
            assert_eq!(s.fence, 88);
            assert_eq!(s.chain, info.p_next as usize);
            assert_eq!(s.swaps, info.p_swapchains as usize);
            assert_eq!(s.indices, info.p_image_indices as usize);
            assert_eq!(s.results, info.p_results as usize);
        });
        assert_eq!(info.p_wait_semaphores, waits.as_ptr());
        assert_eq!(results[0], vk::Result::ERROR_OUT_OF_DATE_KHR);
    }
}
