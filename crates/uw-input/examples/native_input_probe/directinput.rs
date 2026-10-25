//! Independent probe declarations. This module deliberately does not import adapter ABI definitions.
use std::ffi::c_void;
use windows_sys::{
    Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    core::{GUID, w},
};
type Raw = *mut c_void;
type Factory = unsafe extern "system" fn(Raw, u32, *const GUID, *mut Raw, Raw) -> i32;
type CreateDevice = unsafe extern "system" fn(Raw, *const GUID, *mut Raw, Raw) -> i32;
type State = unsafe extern "system" fn(Raw, u32, Raw) -> i32;
type Release = unsafe extern "system" fn(Raw) -> u32;

struct Reference(Raw);
impl Drop for Reference {
    fn drop(&mut self) {
        // SAFETY: each probe reference was returned by a successful system factory/CreateDevice.
        let release = unsafe { std::mem::transmute::<usize, Release>(slot(self.0, 2)) };
        // SAFETY: releases exactly the probe's owned reference, never a game object.
        unsafe { release(self.0) };
    }
}
unsafe fn slot(object: Raw, index: usize) -> usize {
    // SAFETY: caller supplies a live independently selected DirectInput 8 interface and valid SDK slot.
    unsafe { *(*(object.cast::<*const usize>())).add(index) }
}

pub struct Devices {
    devices: Vec<Reference>,
    results: Vec<i32>,
}
impl Devices {
    pub fn create() -> Option<Self> {
        // SAFETY: this probe loads the optional DLL before calling this routine.
        let module = unsafe { GetModuleHandleW(w!("dinput8.dll")) };
        if module.is_null() {
            return None;
        }
        // SAFETY: loaded module and fixed export name from the Windows SDK.
        let export = unsafe { GetProcAddress(module, c"DirectInput8Create".as_ptr().cast()) }.unwrap();
        // SAFETY: independently declared documented export signature.
        let create = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Factory>(export) };
        let mut devices = Vec::new();
        for iid in [
            GUID::from_u128(0xbf798030_483a_4da2_aa99_5d64ed369700),
            GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700),
        ] {
            let mut factory = std::ptr::null_mut();
            // SAFETY: current process instance, SDK IID and own writable output local.
            assert_eq!(
                unsafe { create(GetModuleHandleW(std::ptr::null()), 0x800, &iid, &mut factory, std::ptr::null_mut(),) },
                0
            );
            assert!(!factory.is_null());
            let factory = Reference(factory);
            // SAFETY: both DirectInput 8 factory interfaces have this method at SDK slot 3.
            let device = unsafe { std::mem::transmute::<usize, CreateDevice>(slot(factory.0, 3)) };
            for guid in [
                GUID::from_u128(0x6f1d2b61_d5a0_11cf_bfc7_444553540000),
                GUID::from_u128(0x6f1d2b60_d5a0_11cf_bfc7_444553540000),
            ] {
                let mut output = std::ptr::null_mut();
                // SAFETY: valid owned factory and known keyboard/mouse GUID; no acquisition is requested.
                assert_eq!(unsafe { device(factory.0, &guid, &mut output, std::ptr::null_mut()) }, 0);
                assert!(!output.is_null());
                devices.push(Reference(output));
            }
        }
        let mut probe = Self { devices, results: Vec::new() };
        probe.results = probe.read();
        Some(probe)
    }
    fn read(&self) -> Vec<i32> {
        self.devices
            .iter()
            .map(|device| {
                // SAFETY: valid independently created 8A/8W device; GetDeviceState is slot 9.
                let state = unsafe { std::mem::transmute::<usize, State>(slot(device.0, 9)) };
                let mut buffer = [0u8; 256];
                // SAFETY: writable bounded buffer; an unacquired device must reject the read.
                let result = unsafe { state(device.0, buffer.len() as u32, buffer.as_mut_ptr().cast()) };
                assert!(result < 0, "unacquired device unexpectedly read successfully");
                result
            })
            .collect()
    }
    pub fn verify_unchanged(&self) {
        assert_eq!(self.read(), self.results, "discovery changed pre-existing device behavior");
    }
}
