//! Frame timing. The present path only takes a timestamp and pushes it into a channel;
//! a background thread writes the CSV once per second.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, SystemTime};

use crate::config::{IS_TARGET, csv_name, current_mode, parse_mode, set_mode};

static LAST_PRESENT_NS: AtomicU64 = AtomicU64::new(0);
static TX: OnceLock<Sender<(u64, u64)>> = OnceLock::new();

pub fn on_present() {
    if !*IS_TARGET {
        return;
    }
    let now = monotonic_ns();
    let last = LAST_PRESENT_NS.swap(now, Ordering::Relaxed);
    if last == 0 || now <= last {
        return;
    }
    let _ = TX.get_or_init(spawn_writer).send((now, now - last));
}

fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn spike_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".cache/underwolf-spike")
}

/// The control file's mode and mtime, if it was written after `since`. Files older than
/// the process are ignored, so a leftover file can't change the mode of a new run.
fn read_control(since: SystemTime) -> Option<(SystemTime, crate::config::Mode)> {
    let path = spike_dir().join("control");
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    if modified <= since {
        return None;
    }
    Some((modified, parse_mode(&std::fs::read_to_string(path).ok()?)?))
}

fn open_segment(mode: crate::config::Mode, segment: u32) -> Option<std::fs::File> {
    let path = spike_dir().join(csv_name(std::process::id(), mode, segment));
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

fn spawn_writer() -> Sender<(u64, u64)> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("uw-spike-stats".into())
        .spawn(move || writer(rx));
    tx
}

fn writer(rx: Receiver<(u64, u64)>) {
    let mut control_seen = SystemTime::now();
    let _ = std::fs::create_dir_all(spike_dir());
    let mut mode = current_mode();
    let mut segment = 0;
    let mut file = open_segment(mode, segment);
    let mut buf = String::new();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        loop {
            match rx.try_recv() {
                Ok((present, interval)) => {
                    let _ = writeln!(buf, "{present},{interval}");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(f) = file.as_mut()
            && !buf.is_empty()
        {
            let _ = f.write_all(buf.as_bytes());
        }
        buf.clear();
        // Every control-file write starts a new segment, even if the mode is unchanged.
        if let Some((modified, m)) = read_control(control_seen) {
            control_seen = modified;
            set_mode(m);
            mode = m;
            segment += 1;
            file = open_segment(mode, segment);
        }
    }
}
