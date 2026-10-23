//! The relay: JSON-lines commands from stdin to the bootstrap pipe and replies back as events on
//! stdout, plus the game watcher that reports `game_exited`.

use std::io::{BufRead, Write};

use uw_pipe_proto::jsonl::{Command, Event};
use uw_pipe_proto::pipe::{decode, Message, MAX_PATH_UNITS};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};

use crate::pipe_server::{PipeServer, RELAY_TIMEOUT_MS};

/// Write one event line to stdout (whole lines, so threads never interleave).
pub(crate) fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_line());
    let _ = out.flush();
}

/// Relay host commands to the bootstrap and status replies back. A watcher thread reports
/// `game_exited` the moment the game process ends, even while this blocks on stdin.
pub(crate) fn relay(pipe: &mut PipeServer, pid: u32) {
    spawn_game_watcher(pid);
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match Command::parse(&line) {
            Ok(Command::LoadOverlay { request_id, path }) => {
                if path.encode_utf16().count() > MAX_PATH_UNITS {
                    emit(&Event::reply_error(request_id, "path_too_long"));
                    continue;
                }
                // The command's id travels to the bootstrap, which echoes it in `status`.
                if !pipe.write_message(&Message::LoadOverlay { request_id, path }) {
                    // The bootstrap's end is gone: `bootstrap_disconnected`, not
                    // `game_exited` (design 2a, "Plan 1 items").
                    emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                    emit(&Event::BootstrapDisconnected);
                    return;
                }
                match pipe.read_message(RELAY_TIMEOUT_MS).and_then(|b| decode(&b).ok()) {
                    Some(Message::Status { request_id, code, detail, message }) => {
                        emit(&Event::Status { request_id, code: code.as_str().into(), detail, message });
                    }
                    _ => emit(&Event::reply_error(request_id, "status_timeout")),
                }
            }
            // No overlay channel yet: commands that need the overlay fail (design: no
            // queueing while disconnected).
            Ok(cmd @ (Command::SetState { .. } | Command::Ping { .. })) => {
                emit(&Event::reply_error(cmd.request_id(), "overlay_not_connected"));
            }
            Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
        }
    }
}

/// Watch the game process; when it ends, report `game_exited` and exit (design failure table).
fn spawn_game_watcher(pid: u32) {
    std::thread::spawn(move || {
        // SAFETY: opens the game for SYNCHRONIZE and waits for it to exit.
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return;
            }
            WaitForSingleObject(h, INFINITE);
            CloseHandle(h);
        }
        emit(&Event::GameExited);
        std::process::exit(0);
    });
}
