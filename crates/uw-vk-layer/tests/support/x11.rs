//! Probe-owned Xlib window only; no game-window/input APIs.
use std::ffi::{c_int, c_uint, c_ulong, c_void};
#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const i8) -> *mut c_void;
    fn XDefaultRootWindow(d: *mut c_void) -> c_ulong;
    fn XCreateSimpleWindow(
        d: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border: c_uint,
        border_pixel: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    fn XMapWindow(d: *mut c_void, w: c_ulong) -> c_int;
    fn XStoreName(d: *mut c_void, w: c_ulong, name: *const i8) -> c_int;
    fn XResizeWindow(d: *mut c_void, w: c_ulong, width: c_uint, height: c_uint) -> c_int;
    fn XSync(d: *mut c_void, discard: c_int) -> c_int;
    fn XDestroyWindow(d: *mut c_void, w: c_ulong) -> c_int;
    fn XCloseDisplay(d: *mut c_void) -> c_int;
}
pub struct Window {
    pub display: *mut c_void,
    pub window: c_ulong,
    pub width: u32,
    pub height: u32,
}
impl Window {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        // SAFETY: Xlib owns display/window; all calls occur on this main thread.
        unsafe {
            let display = XOpenDisplay(std::ptr::null());
            if display.is_null() {
                return Err("unavailable: X11 display".into());
            }
            let window = XCreateSimpleWindow(
                display,
                XDefaultRootWindow(display),
                0,
                0,
                width,
                height,
                0,
                0,
                0,
            );
            if window == 0 {
                XCloseDisplay(display);
                return Err("XCreateSimpleWindow".into());
            }
            XStoreName(display, window, c"Underwolf Vulkan probe".as_ptr());
            XMapWindow(display, window);
            XSync(display, 0);
            Ok(Self {
                display,
                window,
                width,
                height,
            })
        }
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        // SAFETY: same live owned window/display, bounded dimensions from CLI.
        unsafe {
            XResizeWindow(self.display, self.window, width, height);
            XSync(self.display, 0);
        }
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: Vulkan surface/device were destroyed before closing this window.
        unsafe {
            XDestroyWindow(self.display, self.window);
            XCloseDisplay(self.display);
        }
    }
}
