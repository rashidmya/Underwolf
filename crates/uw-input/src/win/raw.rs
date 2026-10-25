//! Suppress original-call raw output without changing record count or size-query semantics.
use super::*;
use crate::raw::{ReleaseSet, neutralize};
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    UI::Input::*,
};
static RELEASES: Mutex<Option<ReleaseSet>> = Mutex::new(None);
static DATA: AtomicUsize = AtomicUsize::new(0);
static BUFFER: AtomicUsize = AtomicUsize::new(0);
pub(super) fn targets() -> Vec<hooks::Target> {
    vec![
        hooks::Target {
            address: hooks::resolve(c"GetRawInputData"),
            detour: data as *const () as usize,
            original: &DATA,
        },
        hooks::Target {
            address: hooks::resolve(c"GetRawInputBuffer"),
            detour: buffer as *const () as usize,
            original: &BUFFER,
        },
    ]
}
fn observe(record: &[u8]) -> bool {
    let Some(s) = SHARED.get() else { return false };
    if s.capture() && !s.routing_mode.load(Ordering::Acquire) {
        return true;
    }
    let kind = u32::from_le_bytes(record[..4].try_into().unwrap());
    if kind == 1 && record.len() >= 40 {
        if u32::from_le_bytes(record[36..40].try_into().unwrap()) == crate::raw::RELEASE_TAG {
            return false;
        }
        let mut vk = u16::from_le_bytes(record[30..32].try_into().unwrap()) as i32;
        let scan = u16::from_le_bytes(record[24..26].try_into().unwrap()) as i32;
        let flags = u16::from_le_bytes(record[26..28].try_into().unwrap());
        if vk >= 255 {
            return false;
        }
        vk = match vk {
            16 => {
                if scan == 0x36 {
                    161
                } else {
                    160
                }
            }
            17 => {
                if flags & 2 != 0 {
                    163
                } else {
                    162
                }
            }
            18 => {
                if flags & 2 != 0 {
                    165
                } else {
                    164
                }
            }
            other => other,
        };
        return window::observe(s, InputRecord::key(vk, scan, flags & 1 == 0, flags & 2 != 0, false, 0));
    }
    if kind == 0 && record.len() >= 48 {
        if u32::from_le_bytes(record[44..48].try_into().unwrap()) == crate::raw::RELEASE_TAG {
            return false;
        }
        let flags = u16::from_le_bytes(record[28..30].try_into().unwrap());
        let mut point = windows_sys::Win32::Foundation::POINT::default();
        // SAFETY: real cursor position is converted using a process-owned window and writable point.
        bypass(|| unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(s.hwnd() as HWND, &mut point);
        });
        let mut blocked = s.capture_domain(true);
        if s.routing_mode.load(Ordering::Acquire) {
            blocked |= window::observe(s, InputRecord::motion(point.x, point.y, 0));
        }
        for (button, down) in [1u16, 0x10, 4, 0x40, 0x100].into_iter().enumerate() {
            if flags & down != 0 {
                blocked |= window::observe(s, InputRecord::button(button as i32, true, point.x, point.y, 0));
            }
            if flags & (down << 1) != 0 {
                blocked |= window::observe(s, InputRecord::button(button as i32, false, point.x, point.y, 0));
            }
        }
        if s.routing_mode.load(Ordering::Acquire) {
            let delta = i16::from_le_bytes(record[30..32].try_into().unwrap()) as i32;
            if flags & 0x400 != 0 {
                blocked |= window::observe(s, InputRecord::wheel(0, delta, point.x, point.y, 0));
            }
            if flags & 0x800 != 0 {
                blocked |= window::observe(s, InputRecord::wheel(delta, 0, point.x, point.y, 0));
            }
        }
        return blocked;
    }
    false
}
unsafe fn filter(ptr: *mut u8, bytes: usize) {
    if bytes < 24 {
        return;
    }
    // SAFETY: successful original read initialized the validated span.
    let original = unsafe { std::slice::from_raw_parts(ptr, bytes) };
    if !observe(original) {
        return;
    }
    let Ok(mut pending) = RELEASES.try_lock() else {
        if let Some(s) = SHARED.get() {
            s.revoke(InputReason::Unavailable);
        }
        // Contention must not let the captured input through.
        // SAFETY: the successful original call initialized this checked byte span.
        let record = unsafe { std::slice::from_raw_parts_mut(ptr, bytes) };
        let _ = neutralize(record, &mut ReleaseSet::default());
        return;
    };
    let releases = pending.get_or_insert_with(ReleaseSet::default);
    // SAFETY: the original API initialized exactly this record's validated byte span.
    let record = unsafe { std::slice::from_raw_parts_mut(ptr, bytes) };
    if neutralize(record, releases).is_err()
        && let Some(s) = SHARED.get()
    {
        s.revoke(InputReason::Unavailable)
    }
}
unsafe extern "system" fn data(
    input: HRAWINPUT,
    command: u32,
    out: *mut core::ffi::c_void,
    size: *mut u32,
    header: u32,
) -> u32 {
    boundary(u32::MAX, || {
        // SAFETY: trampoline matches GetRawInputData and is published before enabling.
        let original = unsafe {
            std::mem::transmute::<
                usize,
                unsafe extern "system" fn(HRAWINPUT, u32, *mut core::ffi::c_void, *mut u32, u32) -> u32,
            >(DATA.load(Ordering::Acquire))
        };
        // SAFETY: forward original caller buffers unchanged.
        let result = unsafe { original(input, command, out, size, header) };
        // SAFETY: no pointer arguments.
        let error = unsafe { GetLastError() };
        if !bypassed() && command == RID_INPUT && !out.is_null() && result != u32::MAX && result >= 24 {
            // SAFETY: a successful RID_INPUT read initialized result bytes.
            unsafe { filter(out.cast(), result as usize) }
        }
        // SAFETY: preserve the original API error state.
        unsafe { SetLastError(error) };
        result
    })
}
unsafe extern "system" fn buffer(out: *mut RAWINPUT, size: *mut u32, header: u32) -> u32 {
    boundary(u32::MAX, || {
        // SAFETY: caller provides the required size pointer, or original call handles invalid input.
        let capacity = if size.is_null() { 0 } else { (unsafe { *size }) as usize };
        // SAFETY: trampoline matches the API.
        let original = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut RAWINPUT, *mut u32, u32) -> u32>(
                BUFFER.load(Ordering::Acquire),
            )
        };
        // SAFETY: forward original caller buffers unchanged.
        let result = unsafe { original(out, size, header) };
        // SAFETY: no pointer arguments.
        let error = unsafe { GetLastError() };
        if !bypassed() && !out.is_null() && result != u32::MAX {
            let mut offset = 0usize;
            for _ in 0..result {
                if offset.checked_add(24).is_none_or(|n| n > capacity) {
                    if let Some(s) = SHARED.get() {
                        s.revoke(InputReason::Unavailable)
                    }
                    break;
                }
                // SAFETY: original returned this many records; header lies within the caller's capacity.
                let ptr = unsafe { out.cast::<u8>().add(offset) };
                // SAFETY: initialized header length, read unaligned to avoid assuming caller alignment.
                let len = unsafe { std::ptr::read_unaligned(ptr.add(4).cast::<u32>()) } as usize;
                if len < 24 || offset.checked_add(len).is_none_or(|n| n > capacity) {
                    if let Some(s) = SHARED.get() {
                        s.revoke(InputReason::Unavailable)
                    }
                    break;
                }
                // SAFETY: this initialized record is fully within the original caller's buffer.
                unsafe { filter(ptr, len) };
                offset = (offset + len).div_ceil(8) * 8;
            }
        }
        // SAFETY: preserve original error state after filtering.
        unsafe { SetLastError(error) };
        result
    })
}

pub(super) fn expect_releases(records: &[Option<InputRecord>; 261]) -> bool {
    let Ok(mut slot) = RELEASES.try_lock() else { return false };
    let mut pending = ReleaseSet::default();
    for record in records.iter().flatten() {
        match record.kind {
            1 if (0..256).contains(&record.a) => pending.key_up(record.a as u8),
            4 if (0..5).contains(&record.a) => pending.mouse_up(record.a as u8),
            _ => {}
        }
    }
    *slot = Some(pending);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_raw_size_queries_and_failures_match_originals() {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        // SAFETY: initialize this test thread's UI queue before querying the raw buffer.
        unsafe {
            let mut message = MSG::default();
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        DATA.store(GetRawInputData as *const () as usize, Ordering::Release);
        BUFFER.store(GetRawInputBuffer as *const () as usize, Ordering::Release);
        for header in [1, std::mem::size_of::<RAWINPUTHEADER>() as u32] {
            for hooked in [false, true] {
                let mut size = 0;
                // SAFETY: native null-buffer size query; deliberate invalid handle/header are API errors.
                unsafe {
                    SetLastError(0x1234);
                    let result = if hooked {
                        data(std::ptr::null_mut(), RID_INPUT, std::ptr::null_mut(), &mut size, header)
                    } else {
                        GetRawInputData(std::ptr::null_mut(), RID_INPUT, std::ptr::null_mut(), &mut size, header)
                    };
                    let error = GetLastError();
                    let mut expected_size = 0;
                    SetLastError(0x1234);
                    let expected = GetRawInputData(
                        std::ptr::null_mut(),
                        RID_INPUT,
                        std::ptr::null_mut(),
                        &mut expected_size,
                        header,
                    );
                    assert_eq!((result, size, error), (expected, expected_size, GetLastError()));
                }
            }
            let mut size = 0;
            // SAFETY: both calls use valid size pointers and a documented null output buffer.
            unsafe {
                SetLastError(0x1234);
                let result = buffer(std::ptr::null_mut(), &mut size, header);
                let error = GetLastError();
                let mut expected_size = 0;
                SetLastError(0x1234);
                let expected = GetRawInputBuffer(std::ptr::null_mut(), &mut expected_size, header);
                assert_eq!((result, size, error), (expected, expected_size, GetLastError()));
            }
        }
    }
}
