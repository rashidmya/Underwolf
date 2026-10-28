//! Loader dispatch tables. A present lookup uses try_read and can pass through.
use crate::{client::Runtime, loader_abi::PfnSetDeviceLoaderData};
use ash::vk::{self, Handle};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, LazyLock, Mutex, RwLock},
};
pub fn key<H: Handle>(h: H) -> usize {
    // SAFETY: hooks call this only with live dispatchable loader handles.
    unsafe { *(h.as_raw() as usize as *const usize) }
}
pub unsafe fn cast<F: Copy>(f: vk::PFN_vkVoidFunction) -> Option<F> {
    const {
        assert!(size_of::<F>() == size_of::<unsafe extern "system" fn()>());
    }
    // SAFETY: caller supplies the concrete type matching the resolved name.
    f.map(|f| unsafe { std::mem::transmute_copy(&f) })
}
pub struct InstanceData {
    pub instance: vk::Instance,
    pub gipa: vk::PFN_vkGetInstanceProcAddr,
    pub destroy_instance: vk::PFN_vkDestroyInstance,
    pub render_capable: bool,
}
pub struct DeviceData {
    pub device: vk::Device,
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
    pub queue_families: Mutex<HashMap<u64, u32>>,
    pub runtime: Option<Runtime>,
    pub trace: Option<crate::trace::Trace>,
}
static INSTANCES: LazyLock<RwLock<HashMap<usize, Arc<InstanceData>>>> =
    LazyLock::new(Default::default);
static DEVICES: LazyLock<arc_swap::ArcSwap<HashMap<usize, Arc<DeviceData>>>> =
    LazyLock::new(|| arc_swap::ArcSwap::from_pointee(HashMap::new()));
static DEVICE_WRITER: Mutex<()> = Mutex::new(());
pub fn insert_instance(d: InstanceData) {
    INSTANCES
        .write()
        .unwrap()
        .insert(key(d.instance), Arc::new(d));
}
pub fn remove_instance(k: usize) -> Option<Arc<InstanceData>> {
    INSTANCES.write().unwrap().remove(&k)
}
pub fn instance(k: usize) -> Option<Arc<InstanceData>> {
    INSTANCES.read().unwrap().get(&k).cloned()
}
pub fn instance_for_physical_device(pd: vk::PhysicalDevice) -> Option<Arc<InstanceData>> {
    let map = INSTANCES.read().unwrap();
    map.get(&key(pd)).cloned().or_else(|| {
        if map.len() == 1 {
            map.values().next().cloned()
        } else {
            None
        }
    })
}
pub fn insert_device(d: DeviceData) {
    let _writer = DEVICE_WRITER.lock().unwrap();
    let mut map = (**DEVICES.load()).clone();
    map.insert(key(d.device), Arc::new(d));
    DEVICES.store(Arc::new(map));
}
pub fn remove_device(k: usize) -> Option<Arc<DeviceData>> {
    let _writer = DEVICE_WRITER.lock().unwrap();
    let mut map = (**DEVICES.load()).clone();
    let old = map.remove(&k);
    DEVICES.store(Arc::new(map));
    old
}
pub fn device(k: usize) -> Option<Arc<DeviceData>> {
    DEVICES.load().get(&k).cloned()
}
impl DeviceData {
    pub fn queue_family(&self, q: vk::Queue) -> Result<Option<u32>, ()> {
        Ok(self
            .queue_families
            .try_lock()
            .map_err(|_| ())?
            .get(&q.as_raw())
            .copied())
    }
    pub fn family_supports_graphics(&self, f: u32) -> bool {
        self.queue_family_props
            .get(f as usize)
            .is_some_and(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
    }
}
pub unsafe fn ash_device(gdpa: vk::PFN_vkGetDeviceProcAddr, device: vk::Device) -> ash::Device {
    // SAFETY: caller resolves functions in the next layer for this live device.
    unsafe {
        ash::Device::load_with(
            |name| gdpa(device, name.as_ptr()).map_or(std::ptr::null(), |f| f as *const c_void),
            device,
        )
    }
}
