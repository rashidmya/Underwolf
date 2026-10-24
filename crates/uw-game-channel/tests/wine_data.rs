#![cfg(unix)]
//! The plan 2b gate under plain Wine (system `/usr/bin/wine`, not Proton): two Wine processes in
//! one scratch prefix share a data channel. `data_producer_probe.exe` creates it as the in-game
//! producer will; `data_consumer_probe.exe` reads it as the helper's data thread will. The test
//! drives both, one command at a time, and checks every answer: the snapshot, the producer's own
//! id in the hello block, deltas, events in order, a resume, ring wraparound giving a loss, a busy
//! lock, an abandoned lock with and without a broken header, `disconnect` winning a tie, a channel
//! refused once `connect` is reset (with an object left, and with none), and `Exists` until the
//! consumer lets go.
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
//!   timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_data -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}

fn example(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release/examples")
        .join(name);
    assert!(
        p.exists(),
        "missing {} (build the examples first)",
        p.display()
    );
    p
}

/// Wait up to `d` for `child` to exit, then kill it.
fn wait_bounded(child: &mut Child, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// One probe: commands on its stdin, answers from its stdout on a channel.
struct Probe {
    name: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Probe {
    fn start(dir: &Path, name: &'static str, args: &[&str]) -> Probe {
        let mut child = Command::new(wine())
            .arg(example(&format!("{name}.exe")))
            .args(args)
            .env("WINEPREFIX", dir)
            .env("WINEDEBUG", "-all")
            .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line.trim_end().to_string()).is_err() {
                    break;
                }
            }
        });
        Probe {
            name,
            child,
            stdin,
            lines,
        }
    }

    fn line(&self, within: Duration) -> String {
        self.lines
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("{}: no answer within {within:?}", self.name))
    }

    /// Send one command and return its answer.
    fn ask(&mut self, command: &str) -> String {
        let stdin = self.stdin.as_mut().expect("probe stdin is open");
        writeln!(stdin, "{command}").unwrap();
        stdin.flush().unwrap();
        self.line(Duration::from_secs(30))
    }
}

/// The scratch prefix and both probes. Dropping it (on success or panic) closes their stdin,
/// kills them, ends this prefix's wineserver only, and removes the prefix.
struct Fixture {
    dir: PathBuf,
    probes: Vec<Probe>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for mut p in self.probes.drain(..) {
            drop(p.stdin.take());
            wait_bounded(&mut p.child, Duration::from_secs(5));
        }
        if let Ok(mut k) = Command::new(wineserver())
            .arg("-k")
            .env("WINEPREFIX", &self.dir)
            .spawn()
        {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap_or(&self.dir));
    }
}

fn field<'a>(answer: &'a str, key: &str) -> &'a str {
    answer
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key).and_then(|w| w.strip_prefix('=')))
        .unwrap_or_else(|| panic!("no {key}= in {answer:?}"))
}

/// `<seq>:burst:<seq - 5>` for `seqs` (the burst's data counts from 0 at seq 5).
fn burst(seqs: std::ops::RangeInclusive<u64>) -> String {
    seqs.map(|s| format!(" {s}:burst:{}", s - 5)).collect()
}

/// Catches the design's Win32 objects failing between two Wine processes: the snapshot (nulls
/// included) or a delta not seen, a hello block that carries another producer's id, a first attach
/// replaying the ring instead of starting at the write head, events out of order or twice, a
/// resume that misses or repeats events, ring wraparound not counted as lost, a lock wait longer
/// than 100 ms, an abandoned lock treated as busy or used without re-checking the header,
/// `disconnect` losing a tie to a stale bell, a channel opened although `connect` is not set (a
/// producer still creating it, or one that went away, even with no object left), or a producer
/// re-created while the consumer still holds the old objects.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu examples"]
fn the_data_channel_works_between_two_wine_processes() {
    let dir = std::env::temp_dir()
        .join(format!("uw-inject-datagate-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&dir).unwrap();
    let mut fx = Fixture {
        dir,
        probes: Vec::new(),
    };
    fx.probes
        .push(Probe::start(&fx.dir, "data_producer_probe", &[]));
    // The first run boots the prefix, so allow up to 200 s.
    let ready = fx.probes[0].line(Duration::from_secs(200));
    assert!(ready.starts_with("ready pid="), "producer: {ready:?}");
    let pid = field(&ready, "pid").to_string();
    let producer_id = field(&ready, "producer_id").to_string();
    fx.probes
        .push(Probe::start(&fx.dir, "data_consumer_probe", &[&pid]));
    // A second consumer, only to check that a channel whose `connect` is not set is refused.
    fx.probes
        .push(Probe::start(&fx.dir, "data_consumer_probe", &[&pid]));
    let [producer, consumer, late] = fx.probes.as_mut_slice() else {
        unreachable!("three probes")
    };

    // Attach: the snapshot, and events from the write head (the three pushed before are not
    // replayed).
    assert_eq!(consumer.ask("open"), "opened");
    let attached = consumer.ask("attach");
    let id = field(&attached, "producer_id").to_string();
    assert_ne!(id, "0");
    assert_eq!(
        id, producer_id,
        "the hello block carries the producer's own id"
    );
    assert_eq!(
        attached,
        format!(
            "attached producer_id={id} from_seq=3 schema=probe.a/16,probe.b/16 snapshot=probe.a=1,probe.b=null"
        )
    );
    assert_eq!(consumer.ask("drain"), "drain lost=0");
    // `create()` rang both bells before the consumer opened: take them, so the waits after the
    // delta show that the delta rang its own.
    assert_eq!(consumer.ask("wait"), "wake info");
    assert_eq!(consumer.ask("wait"), "wake events");

    // A delta: both bells ring; the info bell has the smaller index.
    assert_eq!(producer.ask("delta"), "ok");
    assert_eq!(consumer.ask("wait"), "wake info");
    assert_eq!(consumer.ask("changes"), "changes probe.a=2");
    assert_eq!(consumer.ask("wait"), "wake events");
    assert_eq!(consumer.ask("drain"), "drain lost=0 4:fourth:d");
    assert_eq!(consumer.ask("changes"), "changes");

    // A resume from seq 1: every later event once, in order.
    assert_eq!(
        consumer.ask(&format!("attach {id}:1")),
        format!(
            "attached producer_id={id} from_seq=1 schema=probe.a/16,probe.b/16 snapshot=probe.a=2,probe.b=null"
        )
    );
    assert_eq!(
        consumer.ask("drain"),
        "drain lost=0 2:second:b 3:third:c 4:fourth:d"
    );

    // 40 events while the consumer does not read: the ring holds 30, 10 are lost.
    assert_eq!(producer.ask("burst 40"), "ok");
    assert_eq!(
        consumer.ask("drain"),
        format!("drain lost=10{}", burst(15..=44))
    );
    // A resume older than the oldest record: the gap, then the oldest.
    assert!(
        consumer
            .ask(&format!("attach {id}:2"))
            .contains("from_seq=2")
    );
    assert_eq!(
        consumer.ask("drain"),
        format!("drain lost=12{}", burst(15..=44))
    );

    // A busy lock: given up after about 100 ms, and read on the next try.
    assert_eq!(producer.ask("hold-info 1500"), "ok");
    let busy = consumer.ask("changes");
    let ms: u64 = busy
        .strip_prefix("busy ")
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("{busy:?}"));
    assert!((80..1000).contains(&ms), "gave up after {ms} ms");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(consumer.ask("changes"), "changes");

    // An abandoned lock is acquired; with the header intact the read succeeds, with it broken it
    // is a bad layout.
    assert_eq!(producer.ask("abandon-events"), "ok");
    assert_eq!(consumer.ask("drain"), "drain lost=0");
    assert_eq!(producer.ask("abandon-info"), "ok");
    assert_eq!(
        consumer.ask("changes"),
        "error data_bad_layout info store magic is not UWDI"
    );

    // `disconnect` wins the tie with the events bell the burst left set.
    assert_eq!(producer.ask("drop"), "ok");
    assert_eq!(consumer.ask("wait"), "wake disconnect");
    // `connect` is reset: a consumer that opens now refuses the channel, though the first one
    // still holds its objects.
    assert_eq!(late.ask("open-once"), "not ready");
    assert_eq!(producer.ask("recreate"), "exists");
    assert_eq!(consumer.ask("close"), "closed");
    // No object is left now: the channel is not ready at all.
    assert_eq!(late.ask("open-once"), "not ready");
    assert_eq!(producer.ask("recreate"), "ok");
    assert_eq!(late.ask("open-once"), "opened");
}
