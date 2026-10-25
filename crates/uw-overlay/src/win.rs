use super::*;
use uw_game_channel::input::{SessionEvent, start_routing};
use uw_input::win::{InputRuntime, RuntimeEvent};
use uw_pipe_proto::{input::InputReason, pipe::OverlayMessage, routing::RoutingMessage};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    System::{LibraryLoader::*, Threading::GetCurrentProcessId},
    UI::WindowsAndMessaging::*,
};
static STARTED: StartOnce = StartOnce(AtomicBool::new(false));
/// # Safety
/// Bootstrap supplies a readable StartParams allocation, starting with its size field.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn uw_overlay_start(params: *const StartParams) -> u32 {
    std::panic::catch_unwind(|| {
        if params.is_null() {
            return 1;
        }
        // SAFETY: caller provides a readable size field; don't read remaining fields if too short.
        if unsafe { (*params).size } < 12 {
            return 1;
        }
        // SAFETY: caller supplies this-sized parameter block.
        let p = unsafe { &*params };
        // SAFETY: no arguments.
        if validate_params(p.size, p.version, p.pid, unsafe { GetCurrentProcessId() }).is_err() {
            return 1;
        }
        if !STARTED.claim() {
            return 0;
        }
        let mut module = std::ptr::null_mut();
        // SAFETY: FROM_ADDRESS treats our resident function address as address, not string.
        if unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                uw_overlay_start as *const u16,
                &mut module,
            )
        } == 0
        {
            return 2;
        }
        if std::thread::Builder::new().name("uw-overlay".into()).spawn(run).is_err() {
            return 3;
        }
        0
    })
    .unwrap_or(4)
}
unsafe extern "system" fn find(hwnd: HWND, param: LPARAM) -> i32 {
    let mut pid = 0;
    // SAFETY: output pointer is local, window is supplied by enumeration.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    // SAFETY: queries do not dereference foreign memory.
    if pid == unsafe { GetCurrentProcessId() }
        // SAFETY: opaque enumerated window handle is queried only.
        && unsafe { IsWindowVisible(hwnd) } != 0
        // SAFETY: opaque enumerated window handle is queried only.
        && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
    {
        // SAFETY: run passes its writable HWND local as enumeration context.
        unsafe { *(param as *mut HWND) = hwnd };
        return 0;
    }
    1
}
fn run() {
    let mut hwnd: HWND = std::ptr::null_mut();
    // SAFETY: callback and local context stay alive for synchronous enumeration.
    unsafe {
        EnumWindows(Some(find), &mut hwnd as *mut HWND as isize);
    }
    if hwnd.is_null() {
        eprintln!("uw-overlay: no game window");
        return;
    }
    let runtime = match InputRuntime::install(hwnd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("uw-overlay: {e:?}");
            return;
        }
    };
    let handle = runtime.handle();
    let callbacks = handle.clone();
    let endpoint = match start_routing(
        3,
        Box::new(move |event| match event {
            SessionEvent::Connected => callbacks.connect_routing(),
            SessionEvent::Disconnected => callbacks.disconnect(),
            SessionEvent::DeferHidden => callbacks.defer_hidden(),
            SessionEvent::Configure(config) => match uw_input::configuration::PreparedConfig::compile(config) {
                Ok(config) => {
                    let _ = callbacks.configure(config);
                }
                Err(_) => callbacks.revoke(InputReason::Unavailable),
            },
            SessionEvent::RouteRequest(request) => {
                let _ = callbacks.route_request(request);
            }
            // Input-capable state commands carry their own ordered snapshots; legacy bells coalesce.
            SessionEvent::State { state, .. } => callbacks.observe_state_sequence(state.seq),
            SessionEvent::Visibility { state_seq, on } => callbacks.set_visible_state(on, state_seq),
            SessionEvent::ApplyCapture { request, visible, state_seq } => {
                callbacks.apply_capture(request, visible, state_seq);
            }
            SessionEvent::Capture(r) => {
                let _ = callbacks.request(r);
            }
            SessionEvent::Ack(a) => callbacks.acknowledge(a),
            SessionEvent::Ping(_) => {}
        }),
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("uw-overlay: {e}");
            return;
        }
    };
    loop {
        if endpoint.connected() {
            for event in handle.drain() {
                let message = match event {
                    RuntimeEvent::Disconnect => {
                        endpoint.disconnect();
                        break;
                    }
                    RuntimeEvent::State(_) | RuntimeEvent::Result(_) | RuntimeEvent::Batch(_) => continue,
                    RuntimeEvent::Applied(seq) => {
                        if endpoint.applied(seq).is_err() {
                            handle.revoke(InputReason::Overflow)
                        }
                        continue;
                    }
                    RuntimeEvent::RoutingState(state) => RoutingMessage::State(state),
                    RuntimeEvent::Configured(result) => RoutingMessage::Configured(result),
                    RuntimeEvent::RoutedBatch(batch) => RoutingMessage::Events(batch),
                    RuntimeEvent::RoutingResult(result) => RoutingMessage::Result(result),
                    RuntimeEvent::NativeCoverage(coverage) => RoutingMessage::Coverage(coverage),
                };
                if endpoint.try_send(OverlayMessage::Routing(message)).is_err() {
                    handle.revoke(InputReason::Overflow);
                    break;
                }
            }
            if let Some(c) = handle.challenge()
                && endpoint.try_send(OverlayMessage::Routing(RoutingMessage::Challenge(c))).is_err()
            {
                handle.revoke(InputReason::Overflow)
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
