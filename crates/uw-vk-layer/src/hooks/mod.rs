mod device;
mod instance;
mod present;
mod swapchain;
use crate::dispatch::{self, DeviceData};
use ash::vk;
use std::ffi::{CStr, c_char};
pub use {device::*, instance::*, present::*, swapchain::*};
fn as_void(f: *const ()) -> vk::PFN_vkVoidFunction {
    // SAFETY: callers pass only correctly named extern-system Vulkan hooks.
    Some(unsafe { std::mem::transmute::<*const (), unsafe extern "system" fn()>(f) })
}
pub fn instance_hook(name: &CStr) -> vk::PFN_vkVoidFunction {
    as_void(match name.to_bytes() {
        b"vkGetInstanceProcAddr" => crate::vkGetInstanceProcAddr as *const (),
        b"vkCreateInstance" => create_instance as *const (),
        b"vkDestroyInstance" => destroy_instance as *const (),
        b"vkCreateDevice" => create_device as *const (),
        _ => return None,
    })
}
pub fn device_hook(name: &CStr) -> vk::PFN_vkVoidFunction {
    as_void(match name.to_bytes() {
        b"vkGetDeviceProcAddr" => crate::vkGetDeviceProcAddr as *const (),
        b"vkDestroyDevice" => destroy_device as *const (),
        b"vkGetDeviceQueue" => get_device_queue as *const (),
        b"vkGetDeviceQueue2" => get_device_queue2 as *const (),
        b"vkCreateSwapchainKHR" => create_swapchain as *const (),
        b"vkDestroySwapchainKHR" => destroy_swapchain as *const (),
        b"vkQueuePresentKHR" => queue_present as *const (),
        _ => return None,
    })
}
pub fn device_has(d: &DeviceData, n: &CStr) -> bool {
    match n.to_bytes() {
        b"vkGetDeviceQueue2" => d.get_device_queue2.is_some(),
        b"vkCreateSwapchainKHR" => d.create_swapchain.is_some(),
        b"vkDestroySwapchainKHR" => d.destroy_swapchain.is_some(),
        b"vkQueuePresentKHR" => d.queue_present.is_some(),
        _ => true,
    }
}
pub unsafe fn forward_instance(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    let d = dispatch::instance(dispatch::key(instance))?;
    // SAFETY: live loader instance and valid symbol name supplied by loader.
    unsafe { (d.gipa)(instance, name) }
}
pub unsafe fn names<'a>(p: *const *const c_char, n: u32) -> Vec<&'a CStr> {
    if n == 0 {
        return vec![];
    }
    // SAFETY: valid Vulkan creation structure owns n NUL-terminated names.
    unsafe {
        std::slice::from_raw_parts(p, n as usize)
            .iter()
            .map(|&p| CStr::from_ptr(p))
            .collect()
    }
}
pub unsafe fn chain_find(
    mut p: *const std::ffi::c_void,
    s_type: vk::StructureType,
) -> *const std::ffi::c_void {
    // SAFETY: the Vulkan caller supplies a valid pNext chain; inspect only the
    // common immutable prefix, never modify game-owned nodes.
    unsafe {
        while !p.is_null() {
            let node = p.cast::<vk::BaseInStructure<'_>>();
            if (*node).s_type == s_type {
                return p;
            }
            p = (*node).p_next.cast();
        }
    }
    std::ptr::null()
}
