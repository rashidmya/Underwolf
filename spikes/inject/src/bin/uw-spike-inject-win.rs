//! Windows injector: `uw-spike-inject-win <target.exe> <dll path>`.
//!
//! Finds the target process by exe name and makes it load the DLL: the DLL's path is written into
//! the process and a remote thread runs `LoadLibraryW` on it. Nothing else is written. Loading an
//! already-loaded DLL only bumps its reference count, so injecting twice is harmless.
//!
//! Exit codes: 0 loaded, 2 usage, 3 target not running, 4 could not open/write the process,
//! 5 the remote `LoadLibraryW` did not load the DLL, 6 it did not finish within 15 s.

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: uw-spike-inject-win <target.exe> <dll path>");
        std::process::exit(2);
    }
    let code = match win::inject(&args[1], &args[2]) {
        Ok(pid) => {
            println!("[uw-spike-inject-win] loaded {} into {} (pid {pid})", args[2], args[1]);
            0
        }
        Err((code, msg)) => {
            eprintln!("[uw-spike-inject-win] {msg}");
            code
        }
    };
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-spike-inject-win is a Windows program (build with --target x86_64-pc-windows-gnu)");
    std::process::exit(2);
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::time::Duration;

    use uw_spike_inject::{exe_matches, remote_proc_address};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
        Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx};
    use windows::Win32::System::Threading::{
        CreateRemoteThread, OpenProcess, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
        PROCESS_VM_READ, PROCESS_VM_WRITE, WaitForSingleObject,
    };
    use windows::core::{s, w};

    type Fail = (i32, String);
    const TIMED_OUT: i32 = 6;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn wide_to_string(w: &[u16]) -> String {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    }

    fn find_process(exe: &str) -> Option<u32> {
        unsafe {
            let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
            let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap.0, &mut entry).is_ok();
            while ok {
                if exe_matches(&wide_to_string(&entry.szExeFile), exe) {
                    return Some(entry.th32ProcessID);
                }
                ok = Process32NextW(snap.0, &mut entry).is_ok();
            }
            None
        }
    }

    /// Base address of `module` (a file name or path) in process `pid`.
    fn module_base(pid: u32, module: &str) -> Option<usize> {
        let name = module.rsplit(['\\', '/']).next().unwrap_or(module);
        unsafe {
            let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid).ok()?);
            let mut entry = MODULEENTRY32W { dwSize: size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
            let mut ok = Module32FirstW(snap.0, &mut entry).is_ok();
            while ok {
                if exe_matches(&wide_to_string(&entry.szModule), name) {
                    return Some(entry.modBaseAddr as usize);
                }
                ok = Module32NextW(snap.0, &mut entry).is_ok();
            }
            None
        }
    }

    /// `LoadLibraryW` as mapped in the target: same offset inside kernel32 as in our process.
    fn remote_load_library(pid: u32) -> Result<usize, Fail> {
        unsafe {
            let k32 = GetModuleHandleW(w!("kernel32.dll")).map_err(|e| (5, format!("kernel32: {e}")))?;
            let local = GetProcAddress(k32, s!("LoadLibraryW")).ok_or((5, "no LoadLibraryW".to_string()))?;
            let remote_base = module_base(pid, "kernel32.dll").unwrap_or(k32.0 as usize);
            Ok(remote_proc_address(local as usize, k32.0 as usize, remote_base))
        }
    }

    pub fn inject(exe: &str, dll: &str) -> Result<u32, Fail> {
        let pid = find_process(exe).ok_or_else(|| (3, format!("{exe} is not running in this Wine session")))?;
        let path: Vec<u16> = dll.encode_utf16().chain(Some(0)).collect();
        let bytes = path.len() * 2;
        unsafe {
            let access = PROCESS_CREATE_THREAD
                | PROCESS_QUERY_INFORMATION
                | PROCESS_VM_OPERATION
                | PROCESS_VM_WRITE
                | PROCESS_VM_READ;
            let process = Handle(OpenProcess(access, false, pid).map_err(|e| (4, format!("OpenProcess {pid}: {e}")))?);
            let remote = VirtualAllocEx(process.0, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() {
                return Err((4, "VirtualAllocEx failed".to_string()));
            }
            let result = (|| {
                WriteProcessMemory(process.0, remote, path.as_ptr() as *const c_void, bytes, None)
                    .map_err(|e| (4, format!("WriteProcessMemory: {e}")))?;
                let start: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(remote_load_library(pid)?);
                let thread = Handle(
                    CreateRemoteThread(process.0, None, 0, Some(start), Some(remote), 0, None)
                        .map_err(|e| (5, format!("CreateRemoteThread: {e}")))?,
                );
                if WaitForSingleObject(thread.0, 15_000) != WAIT_OBJECT_0 {
                    return Err((TIMED_OUT, "remote LoadLibraryW did not finish within 15 s".to_string()));
                }
                // The thread's exit code is only the low 32 bits of the HMODULE, so check the
                // module list instead.
                for _ in 0..10 {
                    if module_base(pid, dll).is_some() {
                        return Ok(pid);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err((5, format!("{dll} is not loaded in {exe} after LoadLibraryW")))
            })();
            // A thread that is still running may yet read the path, so leave it allocated.
            if !matches!(result, Err((TIMED_OUT, _))) {
                let _ = VirtualFreeEx(process.0, remote, 0, MEM_RELEASE);
            }
            result
        }
    }
}
