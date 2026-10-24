#![cfg(unix)]
//! The whole Linux chain under plain Wine: `uw-inject` finds the test game in a scratch prefix,
//! starts `uw-helper64.exe` in that Wine session, the bootstrap connects, and the test overlay is
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. A helper
//! killed under a running `uw-inject` is restarted, and the overlay gets the reconciled state.
//! The test overlay's producer reaches `uw-inject`'s user through the game-data channel (2b).
//! Ignored by default (needs Wine and the Windows artifacts).
//!
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap \
//!       -p uw-test-overlay -p uw-helper
//!   xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant, SystemTime};

use uw_host::discovery::find_pids;
use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::Event;

fn wine() -> String {
    std::env::var("UW_WINE").unwrap_or_else(|_| "/usr/bin/wine".into())
}
fn wineserver() -> String {
    std::env::var("UW_WINESERVER").unwrap_or_else(|_| "/usr/bin/wineserver".into())
}
fn win_artifact(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-pc-windows-gnu/release")
        .join(name);
    assert!(p.exists(), "missing {} (build the Windows artifacts first)", p.display());
    p
}

struct Fixture {
    prefix: PathBuf,
    game: Child,
    /// The game's Linux pid in THIS prefix, passed to `uw-inject --pid` so it never picks a stray
    /// `uw-test-game.exe` from another prefix (the review's leftover-process hazard).
    game_pid: i32,
}
/// Wait up to `d` for `child` to exit, then kill it (bounded teardown, as in the helper tests).
fn wait_bounded(child: &mut Child, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.game.kill();
        wait_bounded(&mut self.game, Duration::from_secs(10));
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.prefix).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.prefix.parent().unwrap());
    }
}

/// The game's Linux pid running in `prefix`: a `uw-test-game.exe` pid whose `/proc/<pid>/environ`
/// has this prefix's `WINEPREFIX`. `None` until Wine has booted the game.
fn game_pid_in(prefix: &Path) -> Option<i32> {
    let want = format!("WINEPREFIX={}", prefix.display());
    find_pids("uw-test-game.exe").into_iter().find(|pid| {
        std::fs::read(format!("/proc/{pid}/environ"))
            .map(|e| e.split(|&b| b == 0).any(|v| v == want.as_bytes()))
            .unwrap_or(false)
    })
}

/// Start the test game in a fresh prefix and wait until it shows up in `/proc` for this prefix
/// (the first run boots the prefix, which takes a while).
fn start_game(tag: &str) -> Fixture {
    let prefix = std::env::temp_dir()
        .join(format!("uw-inject-chain-{tag}-{}", std::process::id()))
        .join("pfx");
    std::fs::create_dir_all(&prefix).unwrap();
    let game = Command::new(wine())
        .arg(win_artifact("uw-test-game.exe"))
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .spawn()
        .unwrap();
    // The guard owns the game and the prefix before anything can panic: `Child` does not kill on
    // drop, so a panic in the wait below would otherwise leak the game, its wineserver and the
    // prefix.
    let mut fx = Fixture { prefix, game, game_pid: 0 };
    let start = Instant::now();
    fx.game_pid = loop {
        if let Some(p) = game_pid_in(&fx.prefix) {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(180), "uw-test-game.exe never started");
        std::thread::sleep(Duration::from_millis(500));
    };
    fx
}

/// Files starting with `stem` in the prefix users' `%TEMP%`
/// (`drive_c/users/<user>/AppData/Local/Temp`). **No recursion:** `dosdevices/z: -> /` means a
/// recursive walk would follow that symlink out into the whole filesystem and never finish.
fn markers(prefix: &Path, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for user in std::fs::read_dir(prefix.join("drive_c/users")).into_iter().flatten().flatten() {
        let temp = user.path().join("AppData/Local/Temp");
        for e in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(stem) {
                out.push(e.path());
            }
        }
    }
    out
}

/// The one overlay marker in `prefix` and its mtime. The marker is `uw-test-overlay-<pid>.txt`,
/// rewritten (`CREATE_ALWAYS`, same text) on every load in the same game, so a second load keeps
/// the count at 1 but moves the mtime: compare stamps to prove "not loaded again".
fn marker_stamp(prefix: &Path) -> (PathBuf, SystemTime) {
    let found = markers(prefix, "uw-test-overlay-");
    assert_eq!(found.len(), 1, "expected exactly one overlay marker, got {found:?}");
    let modified = std::fs::metadata(&found[0]).and_then(|m| m.modified()).unwrap();
    (found[0].clone(), modified)
}

/// Run `uw-inject --proton <wine-root> --exe uw-test-game.exe …` (it finds the running game by
/// exe name), collect its printed events until one contains `until` (or `within` passes), keep
/// collecting for `linger` after that, then end it with SIGTERM — which, by design, also ends its
/// helper. Events are read on a thread so the deadline always holds.
fn run_inject(fx: &Fixture, until: &str, within: Duration, linger: Duration) -> Vec<String> {
    // `--proton <root>` is any Wine install with `bin/wine`; the system Wine's root is /usr.
    let proton_root = Path::new(&wine()).parent().unwrap().parent().unwrap().to_path_buf();
    let mut child = Command::new(env!("CARGO_BIN_EXE_uw-inject"))
        .args(["--exe", "uw-test-game.exe", "--pid"])
        .arg(fx.game_pid.to_string())
        .arg("--proton")
        .arg(&proton_root)
        .arg("--helper")
        .arg(win_artifact("uw-helper64.exe"))
        .arg("--bootstrap")
        .arg(win_artifact("uw_bootstrap64.dll"))
        .arg("--overlay")
        .arg(win_artifact("uw_test_overlay.dll"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut deadline = Instant::now() + within;
    let mut matched = false;
    let mut events = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(event) => {
                if !matched && event.contains(until) {
                    matched = true;
                    deadline = Instant::now() + linger;
                }
                events.push(event);
            }
            Err(_) => break,
        }
    }
    // SAFETY: signals our own child process.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    wait_bounded(&mut child, Duration::from_secs(10));
    events
}

#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn uw_inject_connects_and_loads_the_overlay() {
    let fx = start_game("load");
    let events = run_inject(&fx, r#""code":"ok""#, Duration::from_secs(200), Duration::ZERO);
    assert!(events.iter().any(|e| e.contains("bootstrap_connected")), "events: {events:?}");
    assert!(events.iter().any(|e| e.contains(r#""code":"ok""#)), "events: {events:?}");
    let before = marker_stamp(&fx.prefix);

    // A second uw-inject run (client restart) reconnects without reloading the overlay. It stays
    // up for a few seconds after `bootstrap_connected`, so a wrongly sent `load_overlay` would have
    // time to report its `status` and rewrite the marker.
    let events2 = run_inject(&fx, "bootstrap_connected", Duration::from_secs(60), Duration::from_secs(3));
    assert!(
        events2.iter().any(|e| e.contains("bootstrap_connected") && e.contains(r#""overlay_loaded":true"#)),
        "events2: {events2:?}"
    );
    assert!(!events2.iter().any(|e| e.contains(r#""event":"status""#)), "reloaded after a restart: {events2:?}");
    assert_eq!(marker_stamp(&fx.prefix), before, "the overlay marker was rewritten (a second load)");
}

/// The Linux pid of the `exe` (argv0 basename) running in `prefix`, if any.
fn pid_in(prefix: &Path, exe: &str) -> Option<i32> {
    let want = format!("WINEPREFIX={}", prefix.display());
    find_pids(exe).into_iter().find(|pid| {
        std::fs::read(format!("/proc/{pid}/environ"))
            .map(|e| e.split(|&b| b == 0).any(|v| v == want.as_bytes()))
            .unwrap_or(false)
    })
}

/// A running `uw-inject` with its stdin open for commands and its events on a channel. Dropping
/// it sends SIGTERM (its helper then exits on its own: its stdin closes).
struct Inject {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    seen: Vec<String>,
}

impl Inject {
    fn start(fx: &Fixture) -> Inject {
        let proton_root = Path::new(&wine()).parent().unwrap().parent().unwrap().to_path_buf();
        let mut child = Command::new(env!("CARGO_BIN_EXE_uw-inject"))
            .args(["--exe", "uw-test-game.exe", "--pid"])
            .arg(fx.game_pid.to_string())
            .arg("--proton")
            .arg(&proton_root)
            .arg("--helper")
            .arg(win_artifact("uw-helper64.exe"))
            .arg("--bootstrap")
            .arg(win_artifact("uw_bootstrap64.dll"))
            .arg("--overlay")
            .arg(win_artifact("uw_test_overlay.dll"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Inject { child, stdin, events: rx, seen: Vec::new() }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("uw-inject stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next event (all of them are kept in `seen` for failure messages).
    fn next(&mut self, deadline: Instant) -> Event {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = self.events.recv_timeout(left).unwrap_or_else(|_| panic!("no event in time; saw {:#?}", self.seen));
        self.seen.push(line.clone());
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"))
    }

    /// Read events until `want` matches one; return it.
    fn until(&mut self, within: Duration, want: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + within;
        loop {
            let e = self.next(deadline);
            if want(&e) {
                return e;
            }
        }
    }
}

impl Drop for Inject {
    fn drop(&mut self) {
        // SAFETY: signals our own child process.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        wait_bounded(&mut self.child, Duration::from_secs(10));
    }
}

/// Read events until the overlay has applied a state: a `state_set` seq and then a bell with
/// `applied_seq` equal to it, with the overlay's state file saying `expect`. Returns the seq.
fn until_applied(inj: &mut Inject, fx: &Fixture, expect: &str, within: Duration) -> u64 {
    let deadline = Instant::now() + within;
    let mut last_set = None;
    loop {
        match inj.next(deadline) {
            Event::StateSet { seq, .. } => last_set = Some(seq),
            Event::Bell { applied_seq, .. } if Some(applied_seq) == last_set => {
                let state = markers(&fx.prefix, "uw-test-state-")
                    .first()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
                if state.contains(expect) {
                    return applied_seq;
                }
            }
            _ => {}
        }
    }
}

/// Catches the design's restart path breaking: a killed helper not restarted, the overlay not
/// reconnecting, the desired state not pushed again (a change made while the helper was down is
/// lost), the overlay loaded twice, or the game harmed.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_killed_helper_is_restarted_and_the_overlay_gets_the_reconciled_state() {
    let fx = start_game("restart");
    let mut inj = Inject::start(&fx);
    inj.until(Duration::from_secs(200), |e| matches!(e, Event::OverlayConnected { .. }));
    // The reconcile push (visible) is applied first, then edit mode on (no request id: assigned).
    until_applied(&mut inj, &fx, "visible=1", Duration::from_secs(30));
    inj.send(r#"{"cmd":"set_state","edit_mode":true}"#);
    let before = until_applied(&mut inj, &fx, "edit_mode=1", Duration::from_secs(30));

    // Kill only the helper. While it is down, turn edit mode off: refused for now, remembered.
    let helper = pid_in(&fx.prefix, "uw-helper64.exe").expect("the helper runs in the prefix");
    // SAFETY: kills the test's own helper process.
    unsafe { libc::kill(helper, libc::SIGKILL) };
    let killed_at = inj.seen.len();
    inj.send(r#"{"cmd":"set_state","edit_mode":false}"#);
    inj.until(Duration::from_secs(30), |e| *e == Event::HelperRestarted { attempt: 1 });
    let connected = inj.until(Duration::from_secs(60), |e| matches!(e, Event::BootstrapConnected { .. }));
    assert!(
        matches!(&connected, Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(p) } if p.ends_with("uw_test_overlay.dll")),
        "{connected:?}"
    );
    // The overlay reconnects on its own; the host pushes the full state, and edit mode goes off.
    inj.until(Duration::from_secs(30), |e| matches!(e, Event::OverlayConnected { .. }));
    let after = until_applied(&mut inj, &fx, "edit_mode=0", Duration::from_secs(30));
    // The game held the state block across the restart, so `seq` went on counting.
    assert!(after > before, "seq {after} after the restart, {before} before");
    // `ping` reaches the reconnected overlay and reports what it applied last.
    inj.send(r#"{"cmd":"ping"}"#);
    let pong = inj.until(Duration::from_secs(10), |e| matches!(e, Event::Pong { .. }));
    assert!(matches!(pong, Event::Pong { seen_seq, .. } if seen_seq == after), "{pong:?}");
    // The pinned bootstrap kept the overlay: no second load.
    assert!(!inj.seen[killed_at..].iter().any(|l| l.contains(r#""event":"status""#)), "reloaded: {:#?}", inj.seen);
    assert!(pid_in(&fx.prefix, "uw-test-game.exe").is_some(), "the game stopped");
}

fn is_data(e: &Event) -> bool {
    matches!(
        e,
        Event::DataConnected { .. }
            | Event::Info { .. }
            | Event::DataEvent { .. }
            | Event::DataLost { .. }
            | Event::DataDisconnected { .. }
    )
}

/// Read events until `done` holds for one; return the game-data events read, in order.
fn data_until(inj: &mut Inject, within: Duration, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    loop {
        let e = inj.next(deadline);
        let stop = done(&e);
        if is_data(&e) {
            seen.push(e);
        }
        if stop {
            return seen;
        }
    }
}

/// The seqs of the `data_event`s in `events`, in order.
fn event_seqs(events: &[Event]) -> Vec<u64> {
    events.iter().filter_map(|e| if let Event::DataEvent { seq, .. } = e { Some(*seq) } else { None }).collect()
}

/// `data_connected` and the snapshot after it: the producer id, `from_seq`, and the snapshot's
/// `info` events.
fn expect_connected(inj: &mut Inject, within: Duration) -> (u64, u64, Vec<Event>) {
    let seen = data_until(inj, within, |e| matches!(e, Event::DataConnected { .. }));
    let Some(Event::DataConnected { channel: 1, producer_id, from_seq, schema }) = seen.last().cloned() else {
        panic!("no data_connected: {seen:?}");
    };
    assert_eq!(schema, [SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])]);
    let snapshot =
        data_until(inj, Duration::from_secs(5), |e| matches!(e, Event::Info { key, .. } if key == "last_seq"));
    (producer_id, from_seq, snapshot)
}

/// Catches the game-data channel not reaching `uw-inject`'s user (design 2b "Testing" 3): no
/// `data_connected` with the test schema, a snapshot without both keys, no `tick` events, no
/// `state_seen` after a `set_state`, or seqs that skip or repeat. Also catches a restarted client
/// (a new `uw-inject`, so a new `uw-host` without a resume point) replaying old events instead of
/// starting at the write head, or a snapshot that does not restore the current values (design
/// "Failures": the whole Linux client restarts).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn uw_inject_relays_the_test_producer_and_a_new_client_starts_at_the_write_head() {
    let fx = start_game("data");
    let last = {
        let mut inj = Inject::start(&fx);
        let (_, from_seq, snapshot) = expect_connected(&mut inj, Duration::from_secs(200));
        let keys: Vec<&str> = snapshot
            .iter()
            .filter_map(|e| if let Event::Info { key, .. } = e { Some(key.as_str()) } else { None })
            .collect();
        assert_eq!(keys, ["ticks", "last_seq"], "{snapshot:?}");
        let mut seen = data_until(&mut inj, Duration::from_secs(10), |e| {
            matches!(e, Event::DataEvent { name, .. } if name == "tick")
        });
        // The host's reconcile applied seq 1; edit mode on applies seq 2.
        inj.send(r#"{"cmd":"set_state","edit_mode":true}"#);
        seen.extend(data_until(&mut inj, Duration::from_secs(20), |e| {
            matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "2")
        }));
        let seqs = event_seqs(&seen);
        let want: Vec<u64> = (from_seq + 1..).take(seqs.len()).collect();
        assert_eq!(seqs, want, "seqs skip or repeat: {seen:?}");
        *seqs.last().unwrap()
    };

    // A new client: no resume point, so the write head, and the snapshot holds the current values.
    let mut inj = Inject::start(&fx);
    let (_, from_seq, snapshot) = expect_connected(&mut inj, Duration::from_secs(60));
    assert!(from_seq >= last, "from_seq {from_seq}, but {last} was delivered before");
    assert!(
        snapshot.iter().any(|e| matches!(e, Event::Info { key, value: Some(_), .. } if key == "last_seq")),
        "the snapshot lost last_seq: {snapshot:?}"
    );
    let seen = data_until(&mut inj, Duration::from_secs(10), |e| matches!(e, Event::DataEvent { .. }));
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}
