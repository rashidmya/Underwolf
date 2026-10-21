//! Plugin loader (`cfg(windows)`), Overwolf-style: on the host's `LoadPlugin` the module loads the
//! DLL with `LoadLibraryW`, resolves `uw_plugin_start`/`uw_plugin_present`/`uw_plugin_stop`, calls
//! start, then calls present about every 16 ms from its own thread. Plugins are never unloaded.

use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, PCWSTR};

type StartFn = unsafe extern "C" fn(u32, *const u8, usize) -> i32;
type PresentFn = unsafe extern "C" fn();

const PRESENT_INTERVAL: Duration = Duration::from_millis(16);

/// Lower-cased paths of the plugins loaded so far.
static LOADED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn export(module: windows::Win32::Foundation::HMODULE, name: &'static [u8]) -> Result<usize, String> {
    // SAFETY: `name` is NUL-terminated.
    unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }
        .map(|f| f as usize)
        .ok_or_else(|| format!("missing export {}", String::from_utf8_lossy(&name[..name.len() - 1])))
}

/// Loads and starts the plugin at `path` (a Windows path) on data channel `channel`.
pub fn load(path: &str, channel: u32, args: &str) -> Result<(), String> {
    let key = path.to_ascii_lowercase();
    let mut loaded = LOADED.lock().unwrap_or_else(|p| p.into_inner());
    if loaded.contains(&key) {
        return Err("already loaded".into());
    }
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL-terminated path. The plugin's DllMain runs here.
    let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.map_err(|e| format!("LoadLibraryW failed: {e}"))?;
    loaded.push(key);
    let start = export(module, b"uw_plugin_start\0")?;
    let present = export(module, b"uw_plugin_present\0")?;
    // Resolved to check the ABI; the spike never unloads a plugin, so it is never called.
    export(module, b"uw_plugin_stop\0")?;
    // SAFETY: the plugin ABI fixes these signatures.
    let (start, present) = unsafe {
        (std::mem::transmute::<usize, StartFn>(start), std::mem::transmute::<usize, PresentFn>(present))
    };
    // SAFETY: `args` is valid for `args.len()` bytes for the duration of the call.
    let r = unsafe { start(channel, args.as_ptr(), args.len()) };
    if r != 0 {
        return Err(format!("uw_plugin_start returned {r}"));
    }
    std::thread::Builder::new()
        .name("uw-plugin-present".into())
        .spawn(move || {
            loop {
                std::thread::sleep(PRESENT_INTERVAL);
                // SAFETY: the plugin stays loaded for the life of the process.
                unsafe { present() };
            }
        })
        .map_err(|e| format!("present thread: {e}"))?;
    Ok(())
}
