//! Microsoft GameInput headers at dce6dabd75b53fcb29db86afe4de5c917921af18.
//! v0 is include/v0, v1/v2 have their own directories, v3 is include/GameInput.h.
use super::Raw;
use windows_sys::core::GUID;
pub const IIDS: [GUID; 4] = [
    GUID::from_u128(0x11be2a7e_4254_445a_9c09_ffc40f006918),
    GUID::from_u128(0x40ffb7e4_6150_407a_b439_132badc08d2d),
    GUID::from_u128(0xbbaa66d2_837a_40f7_a303_917d500955f4),
    GUID::from_u128(0x20efc1c7_5d9a_43ba_b26f_b807fa48609c),
];
pub type Create = unsafe extern "system" fn(*mut Raw) -> i32;
pub type Timestamp = unsafe extern "system" fn(Raw) -> u64;
pub type GetCurrentReading = unsafe extern "system" fn(Raw, u32, Raw, *mut Raw) -> i32;
pub type ReadingCallbackV0 = unsafe extern "system" fn(u64, Raw, Raw, bool);
pub type ReadingCallback = unsafe extern "system" fn(u64, Raw, Raw);
pub type RegisterV0 = unsafe extern "system" fn(Raw, Raw, u32, f32, Raw, ReadingCallbackV0, *mut u64) -> i32;
pub type Register = unsafe extern "system" fn(Raw, Raw, u32, Raw, ReadingCallback, *mut u64) -> i32;
pub type Stop = unsafe extern "system" fn(Raw, u64);
pub type UnregisterV0 = unsafe extern "system" fn(Raw, u64, u64) -> bool;
pub type Unregister = unsafe extern "system" fn(Raw, u64) -> bool;
pub const V0_METHODS: &[(usize, &str, u16)] = &[
    (0, "QueryInterface", 1),
    (1, "AddRef", 2),
    (2, "Release", 3),
    (3, "GetCurrentTimestamp", 100),
    (4, "GetCurrentReading", 101),
    (5, "GetNextReading", 102),
    (6, "GetPreviousReading", 103),
    (7, "GetTemporalReading", 104),
    (8, "RegisterReadingCallback", 105),
    (12, "StopCallback", 106),
    (13, "UnregisterCallback", 107),
];
pub const NEW_METHODS: &[(usize, &str, u16)] = &[
    (0, "QueryInterface", 1),
    (1, "AddRef", 2),
    (2, "Release", 3),
    (3, "GetCurrentTimestamp", 100),
    (4, "GetCurrentReading", 101),
    (5, "GetNextReading", 102),
    (6, "GetPreviousReading", 103),
    (7, "RegisterReadingCallback", 108),
    (11, "StopCallback", 106),
    (12, "UnregisterCallback", 109),
];
