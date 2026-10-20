//! Per-instance and per-device state, keyed by the loader's dispatch key
//! (the first pointer-sized word of every dispatchable handle).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use ash::vk::{self, Handle};

use crate::loader_abi::PfnSetDeviceLoaderData;

pub fn key<H: Handle>(h: H) -> usize {
    // SAFETY: dispatchable handles point at an object whose first word is the dispatch table.
    unsafe { *(h.as_raw() as usize as *const usize) }
}

/// Reinterprets a `PFN_vkVoidFunction` as a concrete Vulkan function pointer type.
///
/// # Safety
/// `F` must be the function pointer type matching the name that produced `f`.
pub unsafe fn cast<F: Copy>(f: vk::PFN_vkVoidFunction) -> Option<F> {
    const { assert!(std::mem::size_of::<F>() == std::mem::size_of::<unsafe extern "system" fn()>()) };
    f.map(|f| unsafe { std::mem::transmute_copy::<unsafe extern "system" fn(), F>(&f) })
}

pub struct InstanceData {
    pub instance: vk::Instance,
    pub gipa: vk::PFN_vkGetInstanceProcAddr,
    pub destroy_instance: vk::PFN_vkDestroyInstance,
}

pub struct DeviceData {
    pub device: vk::Device,
    /// Device functions resolved through the next layer (never back into this layer).
    pub ash: ash::Device,
    pub set_loader_data: Option<PfnSetDeviceLoaderData>,
    pub queue_family_props: Vec<vk::QueueFamilyProperties>,
    pub gdpa: vk::PFN_vkGetDeviceProcAddr,
    pub destroy_device: vk::PFN_vkDestroyDevice,
    pub get_device_queue: vk::PFN_vkGetDeviceQueue,
    pub get_device_queue2: Option<vk::PFN_vkGetDeviceQueue2>,
    pub create_swapchain: Option<vk::PFN_vkCreateSwapchainKHR>,
    pub destroy_swapchain: Option<vk::PFN_vkDestroySwapchainKHR>,
    pub get_swapchain_images: Option<vk::PFN_vkGetSwapchainImagesKHR>,
    pub queue_present: Option<vk::PFN_vkQueuePresentKHR>,
    /// Queue handle (raw) → queue family index.
    pub queue_families: Mutex<HashMap<u64, u32>>,
}

static INSTANCES: LazyLock<RwLock<HashMap<usize, Arc<InstanceData>>>> = LazyLock::new(Default::default);
static DEVICES: LazyLock<RwLock<HashMap<usize, Arc<DeviceData>>>> = LazyLock::new(Default::default);

pub fn insert_instance(data: InstanceData) {
    INSTANCES.write().unwrap().insert(key(data.instance), Arc::new(data));
}

pub fn remove_instance(k: usize) -> Option<Arc<InstanceData>> {
    INSTANCES.write().unwrap().remove(&k)
}

pub fn instance(k: usize) -> Option<Arc<InstanceData>> {
    INSTANCES.read().unwrap().get(&k).cloned()
}

/// Physical devices share their instance's dispatch key; if a loader ever breaks that,
/// fall back to the only instance when there is exactly one.
pub fn instance_for_physical_device(pd: vk::PhysicalDevice) -> Option<Arc<InstanceData>> {
    let map = INSTANCES.read().unwrap();
    map.get(&key(pd))
        .cloned()
        .or_else(|| if map.len() == 1 { map.values().next().cloned() } else { None })
}

pub fn insert_device(data: DeviceData) {
    DEVICES.write().unwrap().insert(key(data.device), Arc::new(data));
}

pub fn remove_device(k: usize) -> Option<Arc<DeviceData>> {
    DEVICES.write().unwrap().remove(&k)
}

/// Queues share their device's dispatch key, so this also resolves a `VkQueue`.
pub fn device(k: usize) -> Option<Arc<DeviceData>> {
    DEVICES.read().unwrap().get(&k).cloned()
}

impl DeviceData {
    pub fn queue_family(&self, queue: vk::Queue) -> Option<u32> {
        self.queue_families.lock().unwrap().get(&queue.as_raw()).copied()
    }

    pub fn family_supports_graphics(&self, family: u32) -> bool {
        self.queue_family_props
            .get(family as usize)
            .is_some_and(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
    }
}
