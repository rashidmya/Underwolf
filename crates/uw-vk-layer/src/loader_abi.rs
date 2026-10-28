//! Hand-written equivalents of the structs in Khronos `vk_layer.h` (loader ↔ layer ABI).
//! `ash` does not ship these.

use std::ffi::{c_char, c_void};

use ash::vk;

pub type PfnGetPhysicalDeviceProcAddr =
    unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiateLayerStructType(pub i32);

impl NegotiateLayerStructType {
    pub const INTERFACE_STRUCT: Self = Self(1);
}

#[repr(C)]
pub struct NegotiateLayerInterface {
    pub s_type: NegotiateLayerStructType,
    pub p_next: *mut c_void,
    pub loader_layer_interface_version: u32,
    pub pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pub pfn_get_physical_device_proc_addr: Option<PfnGetPhysicalDeviceProcAddr>,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerFunction(pub i32);

impl LayerFunction {
    pub const LINK_INFO: Self = Self(0);
    pub const LOADER_DATA_CALLBACK: Self = Self(1);
}

/// Sets the loader dispatch pointer on dispatchable objects the layer creates itself
/// (command buffers), so calls on them dispatch correctly.
pub type PfnSetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

pub const LOADER_INSTANCE_CREATE_INFO: vk::StructureType = vk::StructureType::from_raw(47);
pub const LOADER_DEVICE_CREATE_INFO: vk::StructureType = vk::StructureType::from_raw(48);

#[repr(C)]
pub struct LayerInstanceLink {
    pub p_next: *mut LayerInstanceLink,
    pub pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub pfn_next_get_physical_device_proc_addr: Option<PfnGetPhysicalDeviceProcAddr>,
}

/// The C union's largest member is a struct of two function pointers.
#[repr(C)]
pub union LayerInstanceCreateInfoU {
    pub p_layer_info: *mut LayerInstanceLink,
    _largest: [usize; 2],
}

#[repr(C)]
pub struct LayerInstanceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: LayerFunction,
    pub u: LayerInstanceCreateInfoU,
}

#[repr(C)]
pub struct LayerDeviceLink {
    pub p_next: *mut LayerDeviceLink,
    pub pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub pfn_next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
pub union LayerDeviceCreateInfoU {
    pub p_layer_info: *mut LayerDeviceLink,
    pub pfn_set_device_loader_data: Option<PfnSetDeviceLoaderData>,
}

#[repr(C)]
pub struct LayerDeviceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: LayerFunction,
    pub u: LayerDeviceCreateInfoU,
}

unsafe fn find_link<T>(
    mut p: *const vk::BaseInStructure<'_>,
    s_type: vk::StructureType,
    wanted: LayerFunction,
) -> Option<*mut T> {
    // SAFETY: caller supplies a live loader-owned pNext chain; only the ABI
    // prefix and the matching loader node/union member are accessed.
    unsafe {
        while !p.is_null() {
            if (*p).s_type == s_type {
                // Both loader create-info structs put `function` right after sType/pNext.
                let function = *(p as *const u8)
                    .add(std::mem::size_of::<vk::BaseInStructure>())
                    .cast::<LayerFunction>();
                if function == wanted {
                    return Some(p as *mut T);
                }
            }
            p = (*p).p_next;
        }
        None
    }
}

/// Finds the loader's link-info struct in a `VkInstanceCreateInfo` chain.
pub unsafe fn find_instance_link(
    ci: *const vk::InstanceCreateInfo<'_>,
) -> Option<*mut LayerInstanceCreateInfo> {
    // SAFETY: caller supplies a live loader-owned pNext chain; only the ABI
    // prefix and the matching loader node/union member are accessed.
    unsafe {
        find_link(
            (*ci).p_next.cast(),
            LOADER_INSTANCE_CREATE_INFO,
            LayerFunction::LINK_INFO,
        )
    }
}

/// Finds the loader's link-info struct in a `VkDeviceCreateInfo` chain.
pub unsafe fn find_device_link(
    ci: *const vk::DeviceCreateInfo<'_>,
) -> Option<*mut LayerDeviceCreateInfo> {
    // SAFETY: caller supplies a live loader-owned pNext chain; only the ABI
    // prefix and the matching loader node/union member are accessed.
    unsafe {
        find_link(
            (*ci).p_next.cast(),
            LOADER_DEVICE_CREATE_INFO,
            LayerFunction::LINK_INFO,
        )
    }
}

/// Finds the loader's `vkSetDeviceLoaderData` callback in a `VkDeviceCreateInfo` chain.
pub unsafe fn find_device_loader_data(
    ci: *const vk::DeviceCreateInfo<'_>,
) -> Option<PfnSetDeviceLoaderData> {
    // SAFETY: caller supplies a live loader-owned pNext chain; only the ABI
    // prefix and the matching loader node/union member are accessed.
    unsafe {
        let p: *mut LayerDeviceCreateInfo = find_link(
            (*ci).p_next.cast(),
            LOADER_DEVICE_CREATE_INFO,
            LayerFunction::LOADER_DATA_CALLBACK,
        )?;
        (*p).u.pfn_set_device_loader_data
    }
}
