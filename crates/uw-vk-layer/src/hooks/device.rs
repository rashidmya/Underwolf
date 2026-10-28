use crate::{
    client::Runtime,
    config::IS_TARGET,
    dispatch::{self, DeviceData, InstanceData, cast, key},
    loader_abi::{find_device_link, find_device_loader_data},
    resources::Context,
};
use ash::vk::{self, Handle};
use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
    sync::{Mutex, atomic::Ordering},
};
unsafe fn supported(
    inst: &InstanceData,
    pd: vk::PhysicalDevice,
) -> Option<Vec<vk::ExtensionProperties>> {
    // SAFETY: live instance/device query through the next layer's entrypoint.
    unsafe {
        let f = cast::<vk::PFN_vkEnumerateDeviceExtensionProperties>((inst.gipa)(
            inst.instance,
            c"vkEnumerateDeviceExtensionProperties".as_ptr(),
        ))?;
        let mut n = 0;
        f(pd, std::ptr::null(), &mut n, std::ptr::null_mut())
            .result()
            .ok()?;
        let mut props = vec![vk::ExtensionProperties::default(); n as usize];
        f(pd, std::ptr::null(), &mut n, props.as_mut_ptr())
            .result()
            .ok()?;
        props.truncate(n as usize);
        Some(props)
    }
}
pub unsafe extern "system" fn create_device(
    pd: vk::PhysicalDevice,
    ci: *const vk::DeviceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    // SAFETY: valid arguments supplied by Vulkan's loader.
    unsafe { create_device_inner(pd, ci, alloc, out, *IS_TARGET) }
}
unsafe fn create_device_inner(
    pd: vk::PhysicalDevice,
    ci: *const vk::DeviceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
    target: bool,
) -> vk::Result {
    // SAFETY: valid loader create chain; preserve all game-owned nodes/callbacks.
    unsafe {
        let Some(link_ci) = find_device_link(ci) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*link_ci).u.p_layer_info;
        let next_gipa = (*link).pfn_next_get_instance_proc_addr;
        let gdpa = (*link).pfn_next_get_device_proc_addr;
        let next_link = (*link).p_next;
        (*link_ci).u.p_layer_info = next_link;
        let Some(inst) = dispatch::instance_for_physical_device(pd) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(create) =
            cast::<vk::PFN_vkCreateDevice>(next_gipa(inst.instance, c"vkCreateDevice".as_ptr()))
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let existing = super::names(
            (*ci).pp_enabled_extension_names,
            (*ci).enabled_extension_count,
        );
        let mut extra = vec![];
        let mut capable = false;
        let mut add_feature = false;
        let feature = super::chain_find(
            (*ci).p_next,
            vk::StructureType::PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT,
        )
        .cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>();
        if target
            && inst.render_capable
            && existing.contains(&ash::khr::swapchain::NAME)
            && feature
                .as_ref()
                .is_none_or(|f| f.swapchain_maintenance1 == vk::TRUE)
            && let Some(props) = supported(&inst, pd)
        {
            let wanted = [
                ash::ext::external_memory_host::NAME,
                ash::ext::swapchain_maintenance1::NAME,
            ];
            capable = wanted
                .iter()
                .all(|w| props.iter().any(|p| p.extension_name_as_c_str() == Ok(*w)));
            if capable {
                let features2 = cast::<vk::PFN_vkGetPhysicalDeviceFeatures2>((inst.gipa)(
                    inst.instance,
                    c"vkGetPhysicalDeviceFeatures2".as_ptr(),
                ));
                capable = if let Some(f) = features2 {
                    let mut maintenance =
                        vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default();
                    let mut query =
                        vk::PhysicalDeviceFeatures2::default().push_next(&mut maintenance);
                    f(pd, &mut query);
                    maintenance.swapchain_maintenance1 == vk::TRUE
                } else {
                    false
                };
                if capable {
                    extra.extend(wanted.into_iter().filter(|w| !existing.contains(w)));
                    add_feature = feature.is_null();
                }
            }
        }
        let mut patched = *ci;
        let names: Vec<*const c_char> = existing
            .iter()
            .chain(extra.iter())
            .map(|n| n.as_ptr())
            .collect();
        let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default()
            .swapchain_maintenance1(true);
        if capable {
            patched.enabled_extension_count = names.len() as u32;
            patched.pp_enabled_extension_names = names.as_ptr();
            if add_feature {
                maintenance.p_next = (*ci).p_next.cast_mut();
                patched.p_next = (&maintenance
                    as *const vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>)
                    .cast();
            }
        }
        let mut result = create(pd, if capable { &patched } else { ci }, alloc, out);
        if result != vk::Result::SUCCESS && capable {
            capable = false;
            (*link_ci).u.p_layer_info = next_link;
            result = create(pd, ci, alloc, out);
        }
        if result != vk::Result::SUCCESS {
            return result;
        }
        let device = *out;
        let p = |name: &CStr| gdpa(device, name.as_ptr());
        let ash = dispatch::ash_device(gdpa, device);
        let (Some(destroy_device), Some(get_device_queue)) = (
            cast::<vk::PFN_vkDestroyDevice>(p(c"vkDestroyDevice")),
            cast::<vk::PFN_vkGetDeviceQueue>(p(c"vkGetDeviceQueue")),
        ) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let mut queue_family_props = vec![];
        if let Some(f) = cast::<vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceQueueFamilyProperties".as_ptr(),
        )) {
            let mut n = 0;
            f(pd, &mut n, std::ptr::null_mut());
            queue_family_props.resize(n as usize, vk::QueueFamilyProperties::default());
            f(pd, &mut n, queue_family_props.as_mut_ptr());
            queue_family_props.truncate(n as usize);
        }
        if std::env::var("UW_RENDER_DIAGNOSTICS").as_deref() == Ok("1") {
            eprintln!(
                "[underwolf-render] target={target}, instance={}, device={capable}",
                inst.render_capable
            );
        }
        let runtime = if capable {
            make_context(&inst, pd, ash.clone(), gdpa).and_then(|ctx| Runtime::start(ctx).ok())
        } else {
            None
        };
        dispatch::insert_device(DeviceData {
            device,
            set_loader_data: find_device_loader_data(ci),
            queue_family_props,
            gdpa,
            destroy_device,
            get_device_queue,
            get_device_queue2: cast(p(c"vkGetDeviceQueue2")),
            create_swapchain: cast(p(c"vkCreateSwapchainKHR")),
            destroy_swapchain: cast(p(c"vkDestroySwapchainKHR")),
            get_swapchain_images: cast(p(c"vkGetSwapchainImagesKHR")),
            queue_present: cast(p(c"vkQueuePresentKHR")),
            queue_families: Mutex::new(HashMap::new()),
            runtime,
        });
        result
    }
}
unsafe fn make_context(
    inst: &InstanceData,
    pd: vk::PhysicalDevice,
    device: ash::Device,
    gdpa: vk::PFN_vkGetDeviceProcAddr,
) -> Option<std::sync::Arc<Context>> {
    // SAFETY: supported, enabled features/extensions and live driver dispatch.
    unsafe {
        let mem = cast::<vk::PFN_vkGetPhysicalDeviceMemoryProperties>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceMemoryProperties".as_ptr(),
        ))?;
        let mut props = vk::PhysicalDeviceMemoryProperties::default();
        mem(pd, &mut props);
        let query = cast::<vk::PFN_vkGetPhysicalDeviceProperties2>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceProperties2".as_ptr(),
        ))?;
        let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
        let mut p = vk::PhysicalDeviceProperties2::default().push_next(&mut host);
        query(pd, &mut p);
        // Check importability for the exact source buffer usage/handle type.
        let query_buffer = cast::<vk::PFN_vkGetPhysicalDeviceExternalBufferProperties>((inst
            .gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceExternalBufferProperties".as_ptr(),
        ))?;
        let mut external = vk::ExternalBufferProperties::default();
        query_buffer(
            pd,
            &vk::PhysicalDeviceExternalBufferInfo::default()
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .handle_type(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT),
            &mut external,
        );
        if !external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        {
            return None;
        }
        let host_fns = ash::ext::external_memory_host::DeviceFn::load(|name| {
            gdpa(device.handle(), name.as_ptr()).map_or(std::ptr::null(), |f| f as *const c_void)
        });
        Context::new(
            device,
            host_fns,
            props,
            host.min_imported_host_pointer_alignment.max(4096),
        )
        .ok()
    }
}
pub unsafe extern "system" fn destroy_device(
    device: vk::Device,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    if device == vk::Device::null() {
        return;
    }
    if let Some(d) = dispatch::remove_device(key(device)) {
        if let Some(runtime) = &d.runtime {
            runtime.stop();
        }
        let pins = if let Some(runtime) = &d.runtime {
            // SAFETY: worker is stopped; a valid application device teardown has
            // ordered queue work, and pins are held through the driver call below.
            unsafe { runtime.state.lock().unwrap().teardown() }
        } else {
            vec![]
        };
        // SAFETY: all native workers stopped; unresolved GPU/import resources stay
        // retained until driver teardown returns. No callbacks run on the dead device.
        unsafe { (d.destroy_device)(device, alloc) }
        if let Some(runtime) = &d.runtime {
            runtime
                .state
                .lock()
                .unwrap()
                .ctx
                .life
                .live
                .store(false, Ordering::Release);
        }
        drop(pins);
    }
}
pub unsafe extern "system" fn get_device_queue(
    device: vk::Device,
    family: u32,
    index: u32,
    out: *mut vk::Queue,
) {
    let Some(d) = dispatch::device(key(device)) else {
        return;
    };
    // SAFETY: original valid loader queue arguments and output pointer.
    unsafe {
        (d.get_device_queue)(device, family, index, out);
        if *out != vk::Queue::null() {
            d.queue_families
                .lock()
                .unwrap()
                .insert((*out).as_raw(), family);
        }
    }
}
pub unsafe extern "system" fn get_device_queue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2<'_>,
    out: *mut vk::Queue,
) {
    let Some(d) = dispatch::device(key(device)) else {
        return;
    };
    let Some(next) = d.get_device_queue2 else {
        return;
    };
    // SAFETY: original valid loader queue arguments and output pointer.
    unsafe {
        next(device, info, out);
        if *out != vk::Queue::null() {
            d.queue_families
                .lock()
                .unwrap()
                .insert((*out).as_raw(), (*info).queue_family_index);
            if !(*info).flags.is_empty()
                && let Some(runtime) = &d.runtime
            {
                runtime.state.lock().unwrap().fail(2);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader_abi::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    static SERIAL: Mutex<()> = Mutex::new(());
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static AVAILABLE: AtomicBool = AtomicBool::new(true);
    static FAIL_PATCHED: AtomicBool = AtomicBool::new(false);
    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static DEVICE: AtomicUsize = AtomicUsize::new(0);
    static PATCHED: AtomicBool = AtomicBool::new(false);
    unsafe extern "system" fn dummy_destroy_instance(
        _: vk::Instance,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
    }
    unsafe extern "system" fn dummy_destroy_device(
        _: vk::Device,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
    }
    unsafe extern "system" fn dummy_queue(_: vk::Device, _: u32, _: u32, _: *mut vk::Queue) {}
    unsafe extern "system" fn create(
        _: vk::PhysicalDevice,
        ci: *const vk::DeviceCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Device,
    ) -> vk::Result {
        CALLS.fetch_add(1, Ordering::SeqCst);
        let patched = ci as usize != ORIGINAL.load(Ordering::SeqCst);
        PATCHED.fetch_or(patched, Ordering::SeqCst);
        // SAFETY: mock invocation supplies live local creation/output structures.
        unsafe {
            if patched {
                let names = super::super::names(
                    (*ci).pp_enabled_extension_names,
                    (*ci).enabled_extension_count,
                );
                assert!(names.contains(&ash::ext::external_memory_host::NAME));
                assert!(names.contains(&ash::ext::swapchain_maintenance1::NAME));
                let f = super::super::chain_find(
                    (*ci).p_next,
                    vk::StructureType::PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT,
                )
                .cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>();
                assert_eq!((*f).swapchain_maintenance1, vk::TRUE);
                if FAIL_PATCHED.load(Ordering::SeqCst) {
                    return vk::Result::ERROR_FEATURE_NOT_PRESENT;
                }
            }
            *out = vk::Device::from_raw(DEVICE.load(Ordering::SeqCst) as u64);
        }
        vk::Result::SUCCESS
    }
    unsafe extern "system" fn enumerate(
        _: vk::PhysicalDevice,
        _: *const c_char,
        n: *mut u32,
        p: *mut vk::ExtensionProperties,
    ) -> vk::Result {
        let names = if AVAILABLE.load(Ordering::SeqCst) {
            vec![
                ash::ext::external_memory_host::NAME,
                ash::ext::swapchain_maintenance1::NAME,
            ]
        } else {
            vec![]
        };
        // SAFETY: caller allocated count elements after the first count query.
        unsafe {
            if !p.is_null() {
                for (i, name) in names.iter().enumerate() {
                    let mut property = vk::ExtensionProperties::default();
                    for (dst, src) in property
                        .extension_name
                        .iter_mut()
                        .zip(name.to_bytes_with_nul())
                    {
                        *dst = *src as c_char;
                    }
                    *p.add(i) = property;
                }
            }
            *n = names.len() as u32;
        }
        vk::Result::SUCCESS
    }
    unsafe extern "system" fn features(
        _: vk::PhysicalDevice,
        p: *mut vk::PhysicalDeviceFeatures2<'_>,
    ) {
        // SAFETY: the layer's local query owns this maintenance node.
        unsafe {
            let f = (*p)
                .p_next
                .cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>();
            (*f).swapchain_maintenance1 = vk::TRUE;
        }
    }
    unsafe extern "system" fn gipa(_: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
        // SAFETY: all symbol names are valid and each function matches its ABI.
        unsafe {
            let ptr = match CStr::from_ptr(name).to_bytes() {
                b"vkCreateDevice" => create as *const (),
                b"vkEnumerateDeviceExtensionProperties" => enumerate as *const (),
                b"vkGetPhysicalDeviceFeatures2" => features as *const (),
                _ => return None,
            };
            Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(ptr))
        }
    }
    unsafe extern "system" fn gdpa(_: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
        // SAFETY: mock returns correct concrete signatures for named entrypoints.
        unsafe {
            let ptr = match CStr::from_ptr(name).to_bytes() {
                b"vkDestroyDevice" => dummy_destroy_device as *const (),
                b"vkGetDeviceQueue" => dummy_queue as *const (),
                _ => return None,
            };
            Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(ptr))
        }
    }
    fn run(
        target: bool,
        available: bool,
        disabled: bool,
        fail: bool,
        expected_calls: usize,
        expect_patch: bool,
    ) {
        let _serial = SERIAL.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        AVAILABLE.store(available, Ordering::SeqCst);
        FAIL_PATCHED.store(fail, Ordering::SeqCst);
        PATCHED.store(false, Ordering::SeqCst);
        let instance_word = Box::new(51001usize);
        let device_word = Box::new(51002usize);
        let instance = vk::Instance::from_raw((&*instance_word as *const usize) as u64);
        let pd = vk::PhysicalDevice::from_raw((&*instance_word as *const usize) as u64);
        DEVICE.store((&*device_word as *const usize) as usize, Ordering::SeqCst);
        dispatch::insert_instance(InstanceData {
            instance,
            gipa,
            destroy_instance: dummy_destroy_instance,
            render_capable: true,
        });
        let mut link = LayerDeviceLink {
            p_next: std::ptr::null_mut(),
            pfn_next_get_instance_proc_addr: gipa,
            pfn_next_get_device_proc_addr: gdpa,
        };
        let mut loader = LayerDeviceCreateInfo {
            s_type: LOADER_DEVICE_CREATE_INFO,
            p_next: std::ptr::null(),
            function: LayerFunction::LINK_INFO,
            u: LayerDeviceCreateInfoU {
                p_layer_info: &mut link,
            },
        };
        let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default()
            .swapchain_maintenance1(false);
        if disabled {
            loader.p_next = (&maintenance
                as *const vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>)
                .cast();
        }
        let names = [ash::khr::swapchain::NAME.as_ptr()];
        let mut ci = vk::DeviceCreateInfo::default().enabled_extension_names(&names);
        ci.p_next = (&loader as *const LayerDeviceCreateInfo).cast();
        ORIGINAL.store(
            (&ci as *const vk::DeviceCreateInfo<'_>) as usize,
            Ordering::SeqCst,
        );
        let mut out = vk::Device::null();
        // SAFETY: all mock handles point to live dispatch words; local ABI nodes,
        // output and create structures remain valid throughout the call.
        let r = unsafe { create_device_inner(pd, &ci, std::ptr::null(), &mut out, target) };
        assert_eq!(r, vk::Result::SUCCESS);
        assert_eq!(CALLS.load(Ordering::SeqCst), expected_calls);
        assert_eq!(PATCHED.load(Ordering::SeqCst), expect_patch);
        let data = dispatch::remove_device(key(out)).unwrap();
        assert!(data.runtime.is_none());
        assert!(std::ptr::eq(
            loader.p_next,
            if disabled {
                (&mut maintenance as *mut vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>)
                    .cast()
            } else {
                std::ptr::null()
            }
        ));
        drop(data);
        dispatch::remove_instance(key(instance));
    }
    #[test]
    fn non_target_keeps_original_device_request_and_has_no_worker() {
        run(false, true, false, false, 1, false)
    }
    #[test]
    fn missing_extension_keeps_original_device_request() {
        run(true, false, false, false, 1, false)
    }
    #[test]
    fn explicitly_disabled_game_feature_is_never_overwritten() {
        run(true, true, true, false, 1, false)
    }
    #[test]
    fn patched_device_failure_retries_original_create_request() {
        run(true, true, false, true, 2, true)
    }
}
