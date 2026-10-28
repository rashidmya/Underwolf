//! Bounded present tracing; all file writes belong to its worker.
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Clone, Copy)]
struct Sample {
    at: u64,
    submitted: bool,
}
struct Queue {
    samples: Mutex<VecDeque<Sample>>,
    dropped: AtomicU64,
}
impl Queue {
    fn new() -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(8192)),
            dropped: AtomicU64::new(0),
        }
    }
    fn push(&self, sample: Sample) {
        if let Ok(mut queue) = self.samples.try_lock()
            && queue.len() < 8192
        {
            queue.push_back(sample);
            return;
        }
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
pub struct Trace {
    queue: Arc<Queue>,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
pub struct PresentSample<'a> {
    trace: &'a Trace,
    at: u64,
    pub submitted: bool,
}
impl Drop for PresentSample<'_> {
    fn drop(&mut self) {
        self.trace.queue.push(Sample {
            at: self.at,
            submitted: self.submitted,
        });
    }
}
impl Trace {
    pub fn from_env(device: u64) -> Option<Self> {
        let dir = std::env::var_os("UW_RENDER_TRACE_DIR")?;
        Self::start(Path::new(&dir), device).ok()
    }
    fn start(dir: &Path, device: u64) -> io::Result<Self> {
        let identity = uw_frame_channel::PeerIdentity::current()
            .map_err(|_| io::Error::other("trace identity unavailable"))?;
        std::fs::create_dir_all(dir)?;
        let name = format!("{}-{device}", identity.pid);
        let csv = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(format!("{name}.csv")))?;
        let meta = dir.join(format!("{name}.json"));
        let mut initial = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&meta)?;
        initial.write_all(b"{\"status\":\"running\"}\n")?;
        let queue = Arc::new(Queue::new());
        let stopped = Arc::new(AtomicBool::new(false));
        let shared = queue.clone();
        let stopping = stopped.clone();
        let worker = thread::Builder::new().name("uw-present-trace".into()).spawn(move || {
            let mut writer = BufWriter::new(csv);
            let mut samples = 0u64; let mut submitted = 0u64; let mut previous = None;
            let result = (|| -> io::Result<()> {
                writeln!(writer,"present_ns,interval_ns,submitted")?;
                let mut batch = VecDeque::with_capacity(8192);
                loop {
                    // Move the allocation, not its entries, while holding the lock.
                    { let mut q = shared.samples.lock().unwrap(); std::mem::swap(&mut *q, &mut batch); }
                    for sample in batch.drain(..) {
                        let interval = previous.map_or(0, |p| sample.at.saturating_sub(p));
                        writeln!(writer,"{},{},{}", sample.at, interval, u8::from(sample.submitted))?;
                        previous = Some(sample.at); samples += 1; submitted += u64::from(sample.submitted);
                    }
                    if stopping.load(Ordering::Acquire) && shared.samples.lock().unwrap().is_empty() { break; }
                    writer.flush()?;
                    thread::sleep(Duration::from_millis(10));
                }
                writer.flush()
            })();
            let status = if result.is_ok() { "complete" } else { "failed" };
            let value = format!("{{\"status\":\"{status}\",\"pid\":{},\"startTicks\":\"{}\",\"samples\":{samples},\"submitted\":{submitted},\"skipped\":{},\"dropped\":{}}}\n", identity.pid, identity.start_ticks, samples-submitted, shared.dropped.load(Ordering::Acquire));
            let temporary = meta.with_extension("tmp");
            if std::fs::write(&temporary, value).is_ok() { let _ = std::fs::rename(temporary, meta); }
        })?;
        Ok(Self {
            queue,
            stopped,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn begin(&self) -> PresentSample<'_> {
        PresentSample {
            trace: self,
            at: crate::config::monotonic_ns(),
            submitted: false,
        }
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Trace {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn present_queue_is_bounded_and_contention_drops_instead_of_waiting() {
        let q = Queue::new();
        for n in 0..8193 {
            q.push(Sample {
                at: n,
                submitted: false,
            });
        }
        assert_eq!(q.samples.lock().unwrap().len(), 8192);
        assert_eq!(q.dropped.load(Ordering::Relaxed), 1);
        let _held = q.samples.lock().unwrap();
        q.push(Sample {
            at: 9999,
            submitted: true,
        });
        assert_eq!(q.dropped.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn stopped_writer_flushes_complete_identity_and_real_submission_counts() {
        let dir = std::env::temp_dir().join(format!("uw-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Trace::start(&dir, 71).unwrap();
        t.queue.push(Sample {
            at: 100,
            submitted: false,
        });
        t.queue.push(Sample {
            at: 300,
            submitted: true,
        });
        t.stop();
        let name = format!("{}-71", std::process::id());
        let text = std::fs::read_to_string(dir.join(format!("{name}.csv"))).unwrap();
        assert_eq!(
            text,
            "present_ns,interval_ns,submitted\n100,0,0\n300,200,1\n"
        );
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["status"], "complete");
        assert_eq!(meta["samples"], 2);
        assert_eq!(meta["submitted"], 1);
        assert_eq!(meta["skipped"], 1);
        assert_eq!(meta["dropped"], 0);
        assert_eq!(meta["pid"], std::process::id());
        assert!(meta["startTicks"].as_str().unwrap().parse::<u64>().unwrap() > 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
