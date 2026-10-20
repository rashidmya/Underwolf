//! Intercepted Vulkan entry points. In passthrough mode every hook forwards to the next
//! layer/driver unchanged; only `queue_present` records a timestamp first.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{LazyLock, Mutex};

use ash::vk::{self, Handle};

use crate::client;
use crate::compositor::{Compositor, Upload, widget_view_format};
use crate::config::{IS_TARGET, Mode, current_mode};
use crate::dispatch::{self, DeviceData, InstanceData, cast, key};
use crate::loader_abi::{find_device_link, find_device_loader_data, find_instance_link};
use crate::overlay::{Overlay, Quad, QuadSource};
use crate::present;

/// Swapchain (raw handle) → its overlay renderer.
static OVERLAYS: LazyLock<Mutex<HashMap<u64, Overlay>>> = LazyLock::new(Default::default);

macro_rules! log_once {
    ($($arg:tt)*) => {{
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[underwolf-spike] {}", format!($($arg)*));
        }
    }};
}

fn drawing_enabled() -> bool {
    *IS_TARGET && current_mode() != Mode::Passthrough
}

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
        // External memory on a Vulkan 1.0 instance needs these instance extensions (core in 1.1).
        let mut patched = *ci;
        let ext_ptrs: Vec<*const c_char>;
        let api = (*ci).p_application_info.as_ref().map_or(vk::API_VERSION_1_0, |a| a.api_version);
        if *IS_TARGET && api < vk::API_VERSION_1_1 {
            let existing: Vec<*const c_char> =
                std::slice::from_raw_parts((*ci).pp_enabled_extension_names, (*ci).enabled_extension_count as usize).to_vec();
            let has = |n: &CStr| existing.iter().any(|&p| CStr::from_ptr(p) == n);
            let extra = [ash::khr::external_memory_capabilities::NAME, ash::khr::get_physical_device_properties2::NAME];
            ext_ptrs = existing.iter().copied().chain(extra.iter().filter(|e| !has(e)).map(|e| e.as_ptr())).collect();
            patched.enabled_extension_count = ext_ptrs.len() as u32;
            patched.pp_enabled_extension_names = ext_ptrs.as_ptr();
        }
        let r = next_create(&patched, alloc, out);
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
        // In the target, also enable host-memory import for widget frames when supported.
        let host_import_exts = if *IS_TARGET { host_import_extensions(&inst, physical_device, ci) } else { None };
        let mut patched = *ci;
        let ext_ptrs: Vec<*const c_char>;
        if let Some(extra) = &host_import_exts {
            let existing = std::slice::from_raw_parts((*ci).pp_enabled_extension_names, (*ci).enabled_extension_count as usize);
            ext_ptrs = existing.iter().copied().chain(extra.iter().map(|e| e.as_ptr())).collect();
            patched.enabled_extension_count = ext_ptrs.len() as u32;
            patched.pp_enabled_extension_names = ext_ptrs.as_ptr();
        }
        let r = next_create(physical_device, &patched, alloc, out);
        if r != vk::Result::SUCCESS {
            return r;
        }
        let device = *out;
        let p = |name: &CStr| next_gdpa(device, name.as_ptr());
        let ash = ash::Device::load_with(
            |name| next_gdpa(device, name.as_ptr()).map_or(std::ptr::null(), |f| f as *const c_void),
            device,
        );
        let queue_family_props = match cast::<vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceQueueFamilyProperties".as_ptr(),
        )) {
            Some(f) => {
                let mut n = 0;
                f(physical_device, &mut n, std::ptr::null_mut());
                let mut props = vec![vk::QueueFamilyProperties::default(); n as usize];
                f(physical_device, &mut n, props.as_mut_ptr());
                props.truncate(n as usize);
                props
            }
            None => Vec::new(),
        };
        let (Some(destroy_device), Some(get_device_queue)) = (
            cast::<vk::PFN_vkDestroyDevice>(p(c"vkDestroyDevice")),
            cast::<vk::PFN_vkGetDeviceQueue>(p(c"vkGetDeviceQueue")),
        ) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let compositor = if host_import_exts.is_some() {
            make_compositor(&inst, physical_device, &ash, next_gdpa, device)
        } else {
            if *IS_TARGET {
                log_once!("VK_EXT_external_memory_host unavailable; widgets will not be composited");
            }
            None
        };
        if *IS_TARGET {
            client::start();
        }
        dispatch::insert_device(DeviceData {
            device,
            compositor: Mutex::new(compositor),
            ash,
            set_loader_data: find_device_loader_data(ci),
            queue_family_props,
            gdpa: next_gdpa,
            destroy_device,
            get_device_queue,
            get_device_queue2: cast(p(c"vkGetDeviceQueue2")),
            create_swapchain: cast(p(c"vkCreateSwapchainKHR")),
            destroy_swapchain: cast(p(c"vkDestroySwapchainKHR")),
            get_swapchain_images: cast(p(c"vkGetSwapchainImagesKHR")),
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
        drop(data.compositor.lock().unwrap().take());
        unsafe { (data.destroy_device)(device, alloc) }
    }
}

/// Extensions to append for host-memory import, or `None` if the device can't do it.
unsafe fn host_import_extensions(
    inst: &InstanceData,
    pd: vk::PhysicalDevice,
    ci: *const vk::DeviceCreateInfo<'_>,
) -> Option<Vec<&'static CStr>> {
    unsafe {
        let enumerate = cast::<vk::PFN_vkEnumerateDeviceExtensionProperties>((inst.gipa)(
            inst.instance,
            c"vkEnumerateDeviceExtensionProperties".as_ptr(),
        ))?;
        let mut n = 0;
        enumerate(pd, std::ptr::null(), &mut n, std::ptr::null_mut()).result().ok()?;
        let mut props = vec![vk::ExtensionProperties::default(); n as usize];
        enumerate(pd, std::ptr::null(), &mut n, props.as_mut_ptr()).result().ok()?;
        let supported = |name: &CStr| props.iter().any(|p| p.extension_name_as_c_str() == Ok(name));
        let enabled: Vec<&CStr> = std::slice::from_raw_parts((*ci).pp_enabled_extension_names, (*ci).enabled_extension_count as usize)
            .iter()
            .map(|&p| CStr::from_ptr(p))
            .collect();
        let wanted = [ash::khr::external_memory::NAME, ash::ext::external_memory_host::NAME];
        if !wanted.iter().all(|w| supported(w)) {
            return None;
        }
        Some(wanted.into_iter().filter(|w| !enabled.contains(w)).collect())
    }
}

unsafe fn make_compositor(
    inst: &InstanceData,
    pd: vk::PhysicalDevice,
    ash: &ash::Device,
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    device: vk::Device,
) -> Option<Compositor> {
    unsafe {
        let mem_props_fn = cast::<vk::PFN_vkGetPhysicalDeviceMemoryProperties>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceMemoryProperties".as_ptr(),
        ))?;
        let mut mem_props = vk::PhysicalDeviceMemoryProperties::default();
        mem_props_fn(pd, &mut mem_props);
        let mut align = 4096;
        if let Some(props2) = cast::<vk::PFN_vkGetPhysicalDeviceProperties2>((inst.gipa)(
            inst.instance,
            c"vkGetPhysicalDeviceProperties2".as_ptr(),
        )) {
            let mut host = vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
            let mut p = vk::PhysicalDeviceProperties2::default().push_next(&mut host);
            props2(pd, &mut p);
            if host.min_imported_host_pointer_alignment > 0 {
                align = host.min_imported_host_pointer_alignment;
            }
        }
        let host_fns = ash::ext::external_memory_host::DeviceFn::load(|name| {
            gdpa(device, name.as_ptr()).map_or(std::ptr::null(), |f| f as *const c_void)
        });
        match Compositor::new(ash.clone(), host_fns, mem_props, align) {
            Ok(c) => Some(c),
            Err(e) => {
                log_once!("compositor setup failed: {e:?}");
                None
            }
        }
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
    let Some(d) = dispatch::device(key(device)) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next) = d.create_swapchain else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    unsafe {
        // Overlay resources are created for every target swapchain so the mode can be
        // switched at runtime; passthrough mode just doesn't draw.
        if !*IS_TARGET {
            return next(device, ci, alloc, out);
        }
        // Every surface supports COLOR_ATTACHMENT usage; the overlay renders into the image.
        let mut info = *ci;
        info.image_usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
        let r = next(device, &info, alloc, out);
        if r != vk::Result::SUCCESS {
            return r;
        }
        client::swapchain(info.image_extent.width, info.image_extent.height);
        log_once!("target swapchain format {:?}, {}x{}", info.image_format, info.image_extent.width, info.image_extent.height);
        match make_overlay(&d, *out, &info) {
            Ok(o) => {
                OVERLAYS.lock().unwrap().insert((*out).as_raw(), o);
            }
            Err(e) => log_once!("overlay setup failed ({e:?}); drawing disabled for this swapchain"),
        }
        r
    }
}

unsafe fn make_overlay(
    d: &DeviceData,
    swapchain: vk::SwapchainKHR,
    info: &vk::SwapchainCreateInfoKHR<'_>,
) -> Result<Overlay, vk::Result> {
    let get_images = d.get_swapchain_images.ok_or(vk::Result::ERROR_EXTENSION_NOT_PRESENT)?;
    let set_loader_data = d.set_loader_data.ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
    unsafe {
        let mut n = 0;
        get_images(d.device, swapchain, &mut n, std::ptr::null_mut()).result()?;
        let mut images = vec![vk::Image::null(); n as usize];
        get_images(d.device, swapchain, &mut n, images.as_mut_ptr()).result()?;
        images.truncate(n as usize);
        let set_layout = d.compositor.lock().unwrap().as_ref().map(|c| c.set_layout);
        Overlay::new(d.ash.clone(), set_loader_data, info.image_format, info.image_extent, &images, set_layout)
    }
}

pub unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    // Dropping the overlay waits for the device to go idle before freeing its resources.
    drop(OVERLAYS.lock().unwrap().remove(&swapchain.as_raw()));
    if let Some(next) = dispatch::device(key(device)).and_then(|d| d.destroy_swapchain) {
        unsafe { next(device, swapchain, alloc) }
    }
}

/// Widget frames that arrived since the last present: uploads to record, quads to draw and
/// frame ids to ack after the GPU copy. Outside composite mode frames are acked right away.
struct WidgetWork {
    uploads: Vec<Upload>,
    acks: Vec<u64>,
}

unsafe fn collect_widget_work(d: &DeviceData) -> WidgetWork {
    let mut work = WidgetWork { uploads: Vec::new(), acks: Vec::new() };
    if current_mode() != Mode::Composite {
        client::ack_pending();
        return work;
    }
    let mut guard = d.compositor.lock().unwrap();
    let Some(comp) = guard.as_mut() else {
        client::ack_pending();
        return work;
    };
    // The host went away or reconnected: its widgets are gone.
    let connection = client::connection();
    if comp.connection != connection {
        comp.clear();
        comp.connection = connection;
    }
    let ack_now = |f: &crate::client::PendingFrame| {
        if !f.acked {
            client::ack([f.id]);
        }
    };
    for f in client::take_frames() {
        let Some(slot) = client::slot(f.widget, f.slot) else {
            ack_now(&f);
            continue;
        };
        // Widget images are B8G8R8A8; Electron's Linux bitmaps are BGRA.
        if slot.drm_fourcc != uw_spike_ipc::DRM_FORMAT_ARGB8888 {
            log_once!("unsupported widget pixel format {:#x}; not drawing", slot.drm_fourcc);
            ack_now(&f);
            continue;
        }
        let buffer = match unsafe { comp.import((f.widget, f.slot), &slot) } {
            Ok(b) => b,
            Err(e) => {
                log_once!("shared-memory import failed: {e:?}");
                ack_now(&f);
                continue;
            }
        };
        let w = match unsafe { comp.widget(f.widget, slot.width, slot.height) } {
            Ok(w) => w,
            Err(e) => {
                log_once!("widget image creation failed: {e:?}");
                ack_now(&f);
                continue;
            }
        };
        work.uploads.push(Upload {
            buffer,
            image: w.image,
            width: slot.width,
            height: slot.height,
            stride: slot.stride,
            first: !w.initialized,
        });
        w.initialized = true;
        w.x = f.x;
        w.y = f.y;
        if !f.acked {
            work.acks.push(f.id);
        }
    }
    work
}

fn widget_quads(d: &DeviceData, swapchain_format: vk::Format) -> Vec<Quad> {
    match current_mode() {
        Mode::Quad => vec![Quad { x: 100, y: 100, w: 400, h: 300, source: QuadSource::Solid([0.5, 0.0, 0.25, 0.5]) }],
        Mode::Composite => {
            let guard = d.compositor.lock().unwrap();
            let Some(comp) = guard.as_ref() else { return Vec::new() };
            let srgb = widget_view_format(swapchain_format) == vk::Format::B8G8R8A8_SRGB;
            let mut quads: Vec<_> = comp
                .widgets
                .iter()
                .filter(|(_, w)| w.initialized)
                .map(|(&id, w)| Quad {
                    x: w.x,
                    y: w.y,
                    w: w.width,
                    h: w.height,
                    source: QuadSource::Image {
                        widget: id,
                        view: if srgb { w.view_srgb } else { w.view_unorm },
                        sampler: comp.sampler,
                    },
                })
                .collect();
            quads.sort_by_key(|q| match q.source {
                QuadSource::Image { widget, .. } => widget,
                QuadSource::Solid(_) => 0,
            });
            quads
        }
        Mode::Passthrough => Vec::new(),
    }
}

/// Draws overlays on every swapchain in this present. Returns the semaphore the present
/// must wait on instead of the app's, or `None` to present unchanged.
unsafe fn draw_overlays(d: &DeviceData, queue: vk::Queue, info: &vk::PresentInfoKHR<'_>) -> Option<vk::Semaphore> {
    let Some(family) = d.queue_family(queue) else {
        log_once!("present queue was not obtained through vkGetDeviceQueue*; not drawing");
        client::ack_pending();
        return None;
    };
    if !d.family_supports_graphics(family) {
        log_once!("present queue family {family} has no graphics support; not drawing");
        client::ack_pending();
        return None;
    }
    unsafe {
        let epoch = d.compositor.lock().unwrap().as_mut().map_or(0, |c| c.begin_present());
        let mut work = Some(collect_widget_work(d));
        let app_waits = if info.wait_semaphore_count == 0 || info.p_wait_semaphores.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(info.p_wait_semaphores, info.wait_semaphore_count as usize)
        };
        let swapchains = std::slice::from_raw_parts(info.p_swapchains, info.swapchain_count as usize);
        let indices = std::slice::from_raw_parts(info.p_image_indices, info.swapchain_count as usize);
        let mut overlays = OVERLAYS.lock().unwrap();
        let mut last: Option<vk::Semaphore> = None;
        for (sc, &index) in swapchains.iter().zip(indices) {
            let Some(o) = overlays.get_mut(&sc.as_raw()) else { continue };
            let quads = widget_quads(d, o.format());
            // Uploads (and their acks) go with the first overlay drawn this present.
            let WidgetWork { uploads, acks } = work.take().unwrap_or(WidgetWork { uploads: Vec::new(), acks: Vec::new() });
            let chained = last.map(|s| [s]);
            let wait = chained.as_ref().map_or(app_waits, |s| &s[..]);
            match o.draw(queue, family, index, wait, &quads, &uploads, acks.clone(), epoch) {
                Ok(s) => last = Some(s),
                Err(e) => {
                    log_once!("overlay draw failed: {e:?}");
                    client::ack(acks);
                }
            }
        }
        if let Some(w) = work {
            client::ack(w.acks);
        }
        if let Some(comp) = d.compositor.lock().unwrap().as_mut() {
            comp.sweep(|e| overlays.values().all(|o| o.done_through(e)));
        }
        last
    }
}

pub unsafe extern "system" fn queue_present(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    present::on_present();
    let Some(d) = dispatch::device(key(queue)) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let Some(next) = d.queue_present else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    if *IS_TARGET && current_mode() == Mode::Passthrough {
        // Keep the host's slots flowing; frames stay queued for when compositing turns on.
        client::ack_pending();
    }
    unsafe {
        if drawing_enabled()
            && let Some(sem) = draw_overlays(&d, queue, &*info)
        {
            let waits = [sem];
            let mut patched = *info;
            patched.wait_semaphore_count = 1;
            patched.p_wait_semaphores = waits.as_ptr();
            return next(queue, &patched);
        }
        next(queue, info)
    }
}

/// Used by `vkGetInstanceProcAddr` / `vkGetDeviceProcAddr` exports.
pub unsafe fn forward_instance(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
    let data = dispatch::instance(key(instance))?;
    unsafe { (data.gipa)(instance, name) }
}
