//! `VK_LAYER_UNDERWOLF_spike`: Vulkan implicit layer for the Underwolf spikes.
//! Read/draw-only: it never touches game memory or game logic.

mod config;
mod dispatch;
mod hooks;
mod loader_abi;
mod overlay;
mod present;

use std::ffi::{CStr, c_char};

use ash::vk::{self, Handle};

use loader_abi::{NegotiateLayerInterface, NegotiateLayerStructType};

const LAYER_INTERFACE_VERSION: u32 = 2;

/// # Safety
/// Called by the Vulkan loader with a valid, writable struct.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    v: *mut NegotiateLayerInterface,
) -> vk::Result {
    let Some(v) = (unsafe { v.as_mut() }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    if v.s_type != NegotiateLayerStructType::INTERFACE_STRUCT || v.loader_layer_interface_version < LAYER_INTERFACE_VERSION {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    v.loader_layer_interface_version = LAYER_INTERFACE_VERSION;
    v.pfn_get_instance_proc_addr = Some(vkGetInstanceProcAddr);
    v.pfn_get_device_proc_addr = Some(vkGetDeviceProcAddr);
    v.pfn_get_physical_device_proc_addr = None;
    vk::Result::SUCCESS
}

/// # Safety
/// `name` must be a valid NUL-terminated string; `instance` null or a live instance.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetInstanceProcAddr(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let n = unsafe { CStr::from_ptr(name) };
    if let Some(f) = hooks::instance_hook(n).or_else(|| hooks::device_hook(n)) {
        return Some(f);
    }
    if instance.is_null() {
        return None;
    }
    unsafe { hooks::forward_instance(instance, name) }
}

/// # Safety
/// `name` must be a valid NUL-terminated string; `device` a live device.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceProcAddr(device: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
    if name.is_null() || device.is_null() {
        return None;
    }
    let n = unsafe { CStr::from_ptr(name) };
    let d = dispatch::device(dispatch::key(device))?;
    if let Some(f) = hooks::device_hook(n) {
        return if hooks::device_has(&d, n) { Some(f) } else { None };
    }
    unsafe { (d.gdpa)(device, name) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk;
    use loader_abi::{NegotiateLayerInterface, NegotiateLayerStructType};

    #[test]
    fn exports_resolve_hooks() {
        let f = unsafe { vkGetInstanceProcAddr(vk::Instance::null(), c"vkCreateInstance".as_ptr()) };
        let hook = hooks::create_instance as *const ();
        assert_eq!(f.map(|f| f as *const ()), Some(hook));

        let mut n = NegotiateLayerInterface {
            s_type: NegotiateLayerStructType::INTERFACE_STRUCT,
            p_next: std::ptr::null_mut(),
            loader_layer_interface_version: 2,
            pfn_get_instance_proc_addr: None,
            pfn_get_device_proc_addr: None,
            pfn_get_physical_device_proc_addr: None,
        };
        let r = unsafe { vkNegotiateLoaderLayerInterfaceVersion(&mut n) };
        assert_eq!(r, vk::Result::SUCCESS);
        assert_eq!(n.loader_layer_interface_version, 2);
        assert_eq!(
            n.pfn_get_instance_proc_addr.map(|f| f as *const ()),
            Some(vkGetInstanceProcAddr as *const ())
        );
        assert_eq!(
            n.pfn_get_device_proc_addr.map(|f| f as *const ()),
            Some(vkGetDeviceProcAddr as *const ())
        );
    }

    #[test]
    fn unknown_name_without_instance_is_null() {
        let f = unsafe { vkGetInstanceProcAddr(vk::Instance::null(), c"vkNotAFunction".as_ptr()) };
        assert!(f.is_none());
    }
}
