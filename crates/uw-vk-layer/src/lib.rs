mod policy;
pub use policy::*;

#[cfg(target_os = "linux")]
mod client;
#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod dispatch;
#[cfg(target_os = "linux")]
mod draw;
#[cfg(target_os = "linux")]
mod hooks;
#[cfg(target_os = "linux")]
mod lifetime;
#[cfg(target_os = "linux")]
mod loader_abi;
#[cfg(target_os = "linux")]
mod resources;
#[cfg(target_os = "linux")]
mod trace;

#[cfg(target_os = "linux")]
mod exports {
    use super::{dispatch, hooks, loader_abi};
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
        // SAFETY: loader provides a writable negotiation struct, or null.
        let Some(v) = (unsafe { v.as_mut() }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        if v.s_type != NegotiateLayerStructType::INTERFACE_STRUCT
            || v.loader_layer_interface_version < LAYER_INTERFACE_VERSION
        {
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
    pub unsafe extern "system" fn vkGetInstanceProcAddr(
        instance: vk::Instance,
        name: *const c_char,
    ) -> vk::PFN_vkVoidFunction {
        if name.is_null() {
            return None;
        }
        // SAFETY: non-null NUL-terminated name is guaranteed by the loader.
        let n = unsafe { CStr::from_ptr(name) };
        if let Some(f) = hooks::instance_hook(n).or_else(|| hooks::device_hook(n)) {
            return Some(f);
        }
        if instance.is_null() {
            return None;
        }
        // SAFETY: live loader instance and validated symbol name.
        unsafe { hooks::forward_instance(instance, name) }
    }

    /// # Safety
    /// `name` must be a valid NUL-terminated string; `device` a live device.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn vkGetDeviceProcAddr(
        device: vk::Device,
        name: *const c_char,
    ) -> vk::PFN_vkVoidFunction {
        if name.is_null() || device.is_null() {
            return None;
        }
        // SAFETY: non-null NUL-terminated name is guaranteed by the loader.
        let n = unsafe { CStr::from_ptr(name) };
        let d = dispatch::device(dispatch::key(device))?;
        if let Some(f) = hooks::device_hook(n) {
            return if hooks::device_has(&d, n) {
                Some(f)
            } else {
                None
            };
        }
        // SAFETY: retained device dispatch and valid symbol name.
        unsafe { (d.gdpa)(device, name) }
    }
}
#[cfg(target_os = "linux")]
pub use exports::{
    vkGetDeviceProcAddr, vkGetInstanceProcAddr, vkNegotiateLoaderLayerInterfaceVersion,
};
#[cfg(all(test, target_os = "linux"))]
mod abi_tests {
    use super::*;
    use crate::loader_abi::*;
    use ash::vk;
    #[test]
    fn loader_v2_negotiation_and_entrypoints_resolve() {
        let mut info = NegotiateLayerInterface {
            s_type: NegotiateLayerStructType::INTERFACE_STRUCT,
            p_next: std::ptr::null_mut(),
            loader_layer_interface_version: 2,
            pfn_get_instance_proc_addr: None,
            pfn_get_device_proc_addr: None,
            pfn_get_physical_device_proc_addr: None,
        };
        // SAFETY: valid writable negotiation structure and static C symbol names.
        unsafe {
            assert_eq!(
                vkNegotiateLoaderLayerInterfaceVersion(&mut info),
                vk::Result::SUCCESS
            );
            assert_eq!(info.loader_layer_interface_version, 2);
            assert!(
                vkGetInstanceProcAddr(vk::Instance::null(), c"vkCreateInstance".as_ptr()).is_some()
            );
            assert!(vkGetInstanceProcAddr(vk::Instance::null(), c"vkBogus".as_ptr()).is_none());
        }
    }
}
