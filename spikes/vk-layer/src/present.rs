//! Frame timing. The present path only takes a timestamp and pushes it into a channel;
//! a background thread writes the CSV once per second.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use crate::config::{IS_TARGET, MODE};

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

fn csv_path() -> PathBuf {
    spike_dir().join(format!("frames-{}-{}.csv", std::process::id(), MODE.as_str()))
}

fn spawn_writer() -> Sender<(u64, u64)> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("uw-spike-stats".into())
        .spawn(move || writer(rx));
    tx
}

fn writer(rx: Receiver<(u64, u64)>) {
    let path = csv_path();
    let _ = std::fs::create_dir_all(spike_dir());
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
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
        if !buf.is_empty() {
            let _ = file.write_all(buf.as_bytes());
            buf.clear();
        }
    }
}
