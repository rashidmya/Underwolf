use crate::{
    config::IS_TARGET,
    dispatch::{self, InstanceData, cast, key},
    loader_abi::find_instance_link,
};
use ash::vk;
use std::ffi::c_char;
pub unsafe extern "system" fn create_instance(
    ci: *const vk::InstanceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    // SAFETY: loader ABI supplies valid create/allocator/output pointers and link
    // nodes. The saved next link is restored before a fallback create attempt.
    unsafe {
        let Some(link_ci) = find_instance_link(ci) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*link_ci).u.p_layer_info;
        let next = (*link).pfn_next_get_instance_proc_addr;
        let next_link = (*link).p_next;
        (*link_ci).u.p_layer_info = next_link;
        let Some(create) = cast::<vk::PFN_vkCreateInstance>(next(
            vk::Instance::null(),
            c"vkCreateInstance".as_ptr(),
        )) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let api = (*ci)
            .p_application_info
            .as_ref()
            .map_or(vk::API_VERSION_1_0, |a| a.api_version);
        let existing = super::names(
            (*ci).pp_enabled_extension_names,
            (*ci).enabled_extension_count,
        );
        let mut extra = vec![];
        let mut capable = false;
        if *IS_TARGET
            && api >= vk::API_VERSION_1_1
            && existing.contains(&ash::khr::surface::NAME)
            && let Some(enumerate) = cast::<vk::PFN_vkEnumerateInstanceExtensionProperties>(next(
                vk::Instance::null(),
                c"vkEnumerateInstanceExtensionProperties".as_ptr(),
            ))
        {
            let mut n = 0;
            if enumerate(std::ptr::null(), &mut n, std::ptr::null_mut()) == vk::Result::SUCCESS {
                let mut props = vec![vk::ExtensionProperties::default(); n as usize];
                if enumerate(std::ptr::null(), &mut n, props.as_mut_ptr()) == vk::Result::SUCCESS {
                    let wanted = [
                        ash::ext::surface_maintenance1::NAME,
                        ash::khr::get_surface_capabilities2::NAME,
                    ];
                    capable = wanted.iter().all(|w| {
                        props.iter().any(|p| p.extension_name_as_c_str() == Ok(*w))
                            || existing.contains(w)
                    });
                    if capable {
                        extra.extend(wanted.into_iter().filter(|w| !existing.contains(w)));
                    }
                }
            }
        }
        let mut patched = *ci;
        let pointers: Vec<*const c_char> = existing
            .iter()
            .chain(extra.iter())
            .map(|n| n.as_ptr())
            .collect();
        if !extra.is_empty() {
            patched.enabled_extension_count = pointers.len() as u32;
            patched.pp_enabled_extension_names = pointers.as_ptr();
        }
        let mut result = create(if extra.is_empty() { ci } else { &patched }, alloc, out);
        if result != vk::Result::SUCCESS && !extra.is_empty() {
            capable = false;
            (*link_ci).u.p_layer_info = next_link;
            result = create(ci, alloc, out);
        }
        if result != vk::Result::SUCCESS {
            return result;
        }
        let instance = *out;
        let Some(destroy_instance) =
            cast::<vk::PFN_vkDestroyInstance>(next(instance, c"vkDestroyInstance".as_ptr()))
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        dispatch::insert_instance(InstanceData {
            instance,
            gipa: next,
            destroy_instance,
            render_capable: capable,
        });
        result
    }
}
pub unsafe extern "system" fn destroy_instance(
    instance: vk::Instance,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    if instance == vk::Instance::null() {
        return;
    }
    if let Some(d) = dispatch::remove_instance(key(instance)) {
        // SAFETY: matched live instance; forward the original allocation callbacks.
        unsafe { (d.destroy_instance)(instance, alloc) }
    }
}
