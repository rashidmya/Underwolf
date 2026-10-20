//! Intercepted Vulkan entry points. In passthrough mode every hook forwards to the next
//! layer/driver unchanged; only `queue_present` records a timestamp first.

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::Mutex;

use ash::vk::{self, Handle};

use crate::dispatch::{self, DeviceData, InstanceData, cast, key};
use crate::loader_abi::{find_device_link, find_instance_link};
use crate::present;

fn as_void(f: *const ()) -> vk::PFN_vkVoidFunction {
    // SAFETY: `f` is one of our `extern "system"` hook functions.
    Some(unsafe { std::mem::transmute::<*const (), unsafe extern "system" fn()>(f) })
}

/// Instance-level functions this layer intercepts.
pub fn instance_hook(name: &CStr) -> vk::PFN_vkVoidFunction {
    let f = match name.to_bytes() {
        b"vkGetInstanceProcAddr" => crate::vkGetInstanceProcAddr as *const (),
        b"vkCreateInstance" => create_instance as *const (),
        b"vkDestroyInstance" => destroy_instance as *const (),
        b"vkCreateDevice" => create_device as *const (),
        _ => return None,
    };
    as_void(f)
}

/// Device-level functions this layer intercepts.
pub fn device_hook(name: &CStr) -> vk::PFN_vkVoidFunction {
    let f = match name.to_bytes() {
        b"vkGetDeviceProcAddr" => crate::vkGetDeviceProcAddr as *const (),
        b"vkDestroyDevice" => destroy_device as *const (),
        b"vkGetDeviceQueue" => get_device_queue as *const (),
        b"vkGetDeviceQueue2" => get_device_queue2 as *const (),
        b"vkCreateSwapchainKHR" => create_swapchain as *const (),
        b"vkDestroySwapchainKHR" => destroy_swapchain as *const (),
        b"vkQueuePresentKHR" => queue_present as *const (),
        _ => return None,
    };
    as_void(f)
}

/// Whether the next layer/driver provides `name` on this device. Hooks for optional
/// functions are only handed out when there is something to forward to.
pub fn device_has(d: &DeviceData, name: &CStr) -> bool {
    match name.to_bytes() {
        b"vkGetDeviceQueue2" => d.get_device_queue2.is_some(),
        b"vkCreateSwapchainKHR" => d.create_swapchain.is_some(),
        b"vkDestroySwapchainKHR" => d.destroy_swapchain.is_some(),
        b"vkQueuePresentKHR" => d.queue_present.is_some(),
        _ => true,
    }
}

pub unsafe extern "system" fn create_instance(
    ci: *const vk::InstanceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    unsafe {
        let Some(link_ci) = find_instance_link(ci) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*link_ci).u.p_layer_info;
        let next_gipa = (*link).pfn_next_get_instance_proc_addr;
        // Advance the chain for the next layer.
        (*link_ci).u.p_layer_info = (*link).p_next;

        let Some(next_create) =
            cast::<vk::PFN_vkCreateInstance>(next_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()))
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let r = next_create(ci, alloc, out);
        if r != vk::Result::SUCCESS {
            return r;
        }
        let instance = *out;
        let Some(destroy_instance) =
            cast::<vk::PFN_vkDestroyInstance>(next_gipa(instance, c"vkDestroyInstance".as_ptr()))
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        dispatch::insert_instance(InstanceData { instance, gipa: next_gipa, destroy_instance });
        r
    }
}

pub unsafe extern "system" fn destroy_instance(instance: vk::Instance, alloc: *const vk::AllocationCallbacks<'_>) {
    if instance.is_null() {
        return;
    }
    if let Some(data) = dispatch::remove_instance(key(instance)) {
        unsafe { (data.destroy_instance)(instance, alloc) }
    }
}

pub unsafe extern "system" fn create_device(
    physical_device: vk::PhysicalDevice,
    ci: *const vk::DeviceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    unsafe {
        let Some(link_ci) = find_device_link(ci) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = (*link_ci).u.p_layer_info;
        let next_gipa = (*link).pfn_next_get_instance_proc_addr;
        let next_gdpa = (*link).pfn_next_get_device_proc_addr;
        (*link_ci).u.p_layer_info = (*link).p_next;

        let Some(inst) = dispatch::instance_for_physical_device(physical_device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_create) =
            cast::<vk::PFN_vkCreateDevice>(next_gipa(inst.instance, c"vkCreateDevice".as_ptr()))
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let r = next_create(physical_device, ci, alloc, out);
        if r != vk::Result::SUCCESS {
            return r;
        }
        let device = *out;
        let p = |name: &CStr| next_gdpa(device, name.as_ptr());
        let (Some(destroy_device), Some(get_device_queue)) = (
            cast::<vk::PFN_vkDestroyDevice>(p(c"vkDestroyDevice")),
            cast::<vk::PFN_vkGetDeviceQueue>(p(c"vkGetDeviceQueue")),
        ) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        dispatch::insert_device(DeviceData {
            device,
            gdpa: next_gdpa,
            destroy_device,
            get_device_queue,
            get_device_queue2: cast(p(c"vkGetDeviceQueue2")),
            create_swapchain: cast(p(c"vkCreateSwapchainKHR")),
            destroy_swapchain: cast(p(c"vkDestroySwapchainKHR")),
            queue_present: cast(p(c"vkQueuePresentKHR")),
            queue_families: Mutex::new(HashMap::new()),
        });
        r
    }
}

pub unsafe extern "system" fn destroy_device(device: vk::Device, alloc: *const vk::AllocationCallbacks<'_>) {
    if device.is_null() {
        return;
    }
    if let Some(data) = dispatch::remove_device(key(device)) {
        unsafe { (data.destroy_device)(device, alloc) }
    }
}

pub unsafe extern "system" fn get_device_queue(device: vk::Device, family: u32, index: u32, out: *mut vk::Queue) {
    let Some(d) = dispatch::device(key(device)) else { return };
    unsafe {
        (d.get_device_queue)(device, family, index, out);
        if !(*out).is_null() {
            d.queue_families.lock().unwrap().insert((*out).as_raw(), family);
        }
    }
}

pub unsafe extern "system" fn get_device_queue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2<'_>,
    out: *mut vk::Queue,
) {
    let Some(d) = dispatch::device(key(device)) else { return };
    let Some(next) = d.get_device_queue2 else { return };
    unsafe {
        next(device, info, out);
        if !(*out).is_null() {
            d.queue_families.lock().unwrap().insert((*out).as_raw(), (*info).queue_family_index);
        }
    }
}

pub unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    ci: *const vk::SwapchainCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    let Some(next) = dispatch::device(key(device)).and_then(|d| d.create_swapchain) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    unsafe { next(device, ci, alloc, out) }
}

pub unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    if let Some(next) = dispatch::device(key(device)).and_then(|d| d.destroy_swapchain) {
        unsafe { next(device, swapchain, alloc) }
    }
}

pub unsafe extern "system" fn queue_present(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    present::on_present();
    let Some(next) = dispatch::device(key(queue)).and_then(|d| d.queue_present) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    unsafe { next(queue, info) }
}

/// Used by `vkGetInstanceProcAddr` / `vkGetDeviceProcAddr` exports.
pub unsafe fn forward_instance(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
    let data = dispatch::instance(key(instance))?;
    unsafe { (data.gipa)(instance, name) }
}
