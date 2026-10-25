//! DirectInput 8 ABI from the Windows SDK / MinGW dinput.h. Both A/W have these method signatures.
use super::Raw;
use windows_sys::core::GUID;
pub const IID_A: GUID = GUID::from_u128(0xbf798030_483a_4da2_aa99_5d64ed369700);
pub const IID_W: GUID = GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700);
pub type Create = unsafe extern "system" fn(Raw, u32, *const GUID, *mut Raw, Raw) -> i32;
pub type CreateDevice = unsafe extern "system" fn(Raw, *const GUID, *mut Raw, Raw) -> i32;
pub type Acquire = unsafe extern "system" fn(Raw) -> i32;
pub type GetDeviceState = unsafe extern "system" fn(Raw, u32, Raw) -> i32;
pub type GetDeviceData = unsafe extern "system" fn(Raw, u32, Raw, *mut u32, u32) -> i32;
pub const METHODS: &[(usize, &str, u16)] = &[
    (0, "QueryInterface", 1),
    (1, "AddRef", 2),
    (2, "Release", 3),
    (7, "Acquire", 20),
    (8, "Unacquire", 21),
    (9, "GetDeviceState", 22),
    (10, "GetDeviceData", 23),
    (11, "SetDataFormat", 24),
    (13, "SetCooperativeLevel", 25),
];
