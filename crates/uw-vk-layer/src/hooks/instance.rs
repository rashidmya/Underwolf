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
    // SAFETY: original loader arguments carry the native creation contract.
    unsafe { create_instance_inner(ci, alloc, out, *IS_TARGET) }
}
unsafe fn create_instance_inner(
    ci: *const vk::InstanceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
    target: bool,
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
        if target && api >= vk::API_VERSION_1_1 && existing.contains(&ash::khr::surface::NAME) {
            let wanted = [
                ash::ext::surface_maintenance1::NAME,
                ash::khr::get_surface_capabilities2::NAME,
            ];
            if wanted.iter().all(|name| existing.contains(name)) {
                capable = true;
            } else {
                // Global commands need not be returned by the next layer's
                // GIPA(NULL). Query the loader's global enumeration directly;
                // this creates no instance and cannot recurse into our create.
                let supported = ash::Entry::load()
                    .ok()
                    .and_then(|entry| entry.enumerate_instance_extension_properties(None).ok());
                if let Some(supported) = supported {
                    capable = wanted.iter().all(|name| {
                        existing.contains(name)
                            || supported
                                .iter()
                                .any(|p| p.extension_name_as_c_str() == Ok(*name))
                    });
                    if capable {
                        extra.extend(wanted.into_iter().filter(|name| !existing.contains(name)));
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader_abi::*;
    use ash::vk::Handle;
    use std::ffi::CStr;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    static INSTANCE: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn create(
        _: *const vk::InstanceCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Instance,
    ) -> vk::Result {
        // SAFETY: test owns the output and its fake dispatch word.
        unsafe {
            *out = vk::Instance::from_raw(INSTANCE.load(Ordering::SeqCst) as u64);
        }
        vk::Result::SUCCESS
    }
    unsafe extern "system" fn destroy(_: vk::Instance, _: *const vk::AllocationCallbacks<'_>) {}
    unsafe extern "system" fn gipa(_: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
        // SAFETY: known symbol names map to exact mock ABIs; the next layer has
        // no global extension-enumeration entrypoint, as seen on the real driver.
        unsafe {
            let f = match CStr::from_ptr(name).to_bytes() {
                b"vkCreateInstance" => create as *const (),
                b"vkDestroyInstance" => destroy as *const (),
                _ => return None,
            };
            Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>(f))
        }
    }
    #[test]
    fn already_enabled_instance_dependencies_do_not_require_next_layer_global_enumeration() {
        let _serial = SERIAL.lock().unwrap();
        let word = Box::new(0x778821usize);
        INSTANCE.store((&*word as *const usize) as usize, Ordering::SeqCst);
        let mut link = LayerInstanceLink {
            p_next: std::ptr::null_mut(),
            pfn_next_get_instance_proc_addr: gipa,
            pfn_next_get_physical_device_proc_addr: None,
        };
        let loader = LayerInstanceCreateInfo {
            s_type: LOADER_INSTANCE_CREATE_INFO,
            p_next: std::ptr::null(),
            function: LayerFunction::LINK_INFO,
            u: LayerInstanceCreateInfoU {
                p_layer_info: &mut link,
            },
        };
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
        let extensions = [
            ash::khr::surface::NAME.as_ptr(),
            ash::ext::surface_maintenance1::NAME.as_ptr(),
            ash::khr::get_surface_capabilities2::NAME.as_ptr(),
        ];
        let mut ci = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&extensions);
        ci.p_next = (&loader as *const LayerInstanceCreateInfo).cast();
        let mut out = vk::Instance::null();
        // SAFETY: all mock loader nodes/dispatch words remain live in this scope.
        let result = unsafe { create_instance_inner(&ci, std::ptr::null(), &mut out, true) };
        assert_eq!(result, vk::Result::SUCCESS);
        let instance = dispatch::remove_instance(key(out)).unwrap();
        assert!(instance.render_capable);
    }
}
