//! The data thread (design 2b "Components and flow", "Threads"): the helper's only reader of the
//! game-data channel, on its own thread outside the relay loop. It shares only `emit()` with the
//! relay, which writes each line whole under one lock.
//!
//! 1. Every second, try to open the channel (`Consumer::open`: `connect` must be set).
//! 2. Attach: check the hello block and both regions, take the snapshot, and pick where events
//!    start (after the resume point if it names this producer, else at the write head). Emit
//!    `data_connected`, then one `info` per key in the snapshot.
//! 3. Wait on `[disconnect, info-bell, events-bell]` with a 1 s timeout: emit the changed keys on
//!    `info-bell`, drain the ring on `events-bell` (`data_lost` first when events were lost), and
//!    read both on a timeout, because a signal can be missed while a lock attempt times out. A
//!    read that a busy lock skipped is retried on the next pass, whatever wakes it
//!    (`uw_helper::data_pass`).
//! 4. On `disconnect`, read both regions once more (what the producer wrote just before it went),
//!    then emit `data_disconnected`, close everything and go back to 1.
//!
//! A version or a layout that does not fit is reported once per producer (`error
//! data_version_mismatch` / `data_bad_layout`, `detail`: the channel id); then the thread stops
//! reading and waits for `disconnect`. A panic in the thread exits the helper with code 3, so
//! `uw-host` restarts it with `--data-resume`.

use uw_game_channel::data::{ConsumeError, Consumer, OpenError, Wake};
use uw_helper::data_pass::{Owed, Woke};
use uw_pipe_proto::data::{InfoValue, ReadError, ResumePoint};
use uw_pipe_proto::jsonl::Event;

use windows_sys::Win32::System::Threading::Sleep;

use crate::relay::emit;

/// How often a missing channel is looked for again (design flow step 2).
const RETRY_MS: u32 = 1000;
/// The longest wait for a bell; a timeout reads both regions anyway.
const WAIT_MS: u32 = 1000;
/// The helper's exit code when the data thread panics (its other exits use 0, 1 and 2).
const PANIC_EXIT: i32 = 3;

/// Start the data thread for channel `channel` of the game `pid`. `resume` is `--data-resume`.
pub(crate) fn spawn(pid: u32, channel: u32, resume: Option<ResumePoint>) {
    let started = std::thread::Builder::new()
        .name("uw-data".into())
        .spawn(move || {
            // A panic would end the data relay for the rest of the helper's life: exit instead,
            // so `uw-host` restarts the helper with `--data-resume` (design 2b "Failures"). The
            // panic message is on stderr already.
            if std::panic::catch_unwind(move || run(pid, channel, resume)).is_err() {
                eprintln!("[uw-helper64] the data thread (uw-data) panicked; exiting {PANIC_EXIT}");
                // Take stdout first, so the exit never cuts a line the relay is writing.
                let _whole_lines = std::io::stdout().lock();
                std::process::exit(PANIC_EXIT);
            }
        });
    if let Err(e) = started {
        eprintln!("[uw-helper64] starting the data thread: {e}");
    }
}

fn info(channel: u32, v: InfoValue) -> Event {
    Event::Info {
        channel,
        category: v.category,
        key: v.key,
        value: v.value,
        ts: v.ts,
    }
}

fn report(channel: u32, e: &ReadError) {
    eprintln!("[uw-helper64] data channel {channel}: {e}");
    emit(&Event::data_error(e.code(), channel));
}

fn retry_later() {
    // SAFETY: a plain sleep.
    unsafe { Sleep(RETRY_MS) };
}

fn run(pid: u32, channel: u32, mut resume: Option<ResumePoint>) {
    let mut last_open_error = String::new();
    loop {
        let mut c = match Consumer::open(pid, channel) {
            Ok(c) => c,
            Err(e) => {
                // No producer yet is the normal wait; anything else is logged once.
                if let OpenError::Win32(_) = e
                    && e.to_string() != last_open_error
                {
                    last_open_error = e.to_string();
                    eprintln!("[uw-helper64] data channel {channel}: {last_open_error}");
                }
                retry_later();
                continue;
            }
        };
        match c.attach(resume) {
            Ok(a) => {
                emit(&Event::DataConnected {
                    channel,
                    producer_id: a.producer_id,
                    from_seq: a.from_seq,
                    schema: a.schema.categories().to_vec(),
                });
                for v in a.snapshot {
                    emit(&info(channel, v));
                }
                let mut point = ResumePoint {
                    producer_id: a.producer_id,
                    seq: a.from_seq,
                };
                let ended = relay(&mut c, channel, &mut point);
                resume = Some(point);
                if let Err(e) = ended {
                    report(channel, &e);
                    wait_for_disconnect(&c);
                }
                emit(&Event::DataDisconnected { channel });
            }
            // A lock held over 100 ms: drop everything and try again next pass.
            Err(ConsumeError::Busy) => retry_later(),
            Err(ConsumeError::Read(e)) => {
                report(channel, &e);
                wait_for_disconnect(&c);
            }
        }
        // `c` drops here, after its last wait returned: every handle closes, so the producer's
        // objects go away and a new producer can create them again.
    }
}

/// Relay one producer until `disconnect` (`Ok`) or a read that does not fit (`Err`). `point`
/// follows the last event delivered.
fn relay(c: &mut Consumer, channel: u32, point: &mut ResumePoint) -> Result<(), ReadError> {
    let mut owed = Owed::default();
    loop {
        let woke = match c.wait(WAIT_MS) {
            // The producer is gone, but the views stay valid while `c` holds the handles: read
            // both regions once more, so what it wrote just before it went (say, while this
            // thread was blocked in `emit()`) still comes. A lock busy here is skipped as on any
            // pass, and nothing is owed after it.
            Wake::Disconnect => return read(c, channel, point, &mut owed, (true, true)),
            Wake::Info => Woke::InfoBell,
            Wake::Events => Woke::EventsBell,
            Wake::Timeout => Woke::Timeout,
        };
        let reads = owed.reads(woke);
        read(c, channel, point, &mut owed, reads)?;
    }
}

/// One pass's reads, `(info, events)`: emit the changed keys, then `data_lost` (if events were
/// lost) and the events. A read that a busy lock skipped is marked in `owed`.
fn read(
    c: &mut Consumer,
    channel: u32,
    point: &mut ResumePoint,
    owed: &mut Owed,
    (read_info, read_events): (bool, bool),
) -> Result<(), ReadError> {
    if read_info {
        match c.changes() {
            Ok(values) => {
                owed.info = false;
                values.into_iter().for_each(|v| emit(&info(channel, v)));
            }
            // A busy lock: skip, and read again on the next pass, whatever wakes it.
            Err(ConsumeError::Busy) => owed.info = true,
            Err(ConsumeError::Read(e)) => return Err(e),
        }
    }
    if read_events {
        match c.drain() {
            Ok(d) => {
                owed.events = false;
                if d.lost > 0 {
                    emit(&Event::DataLost {
                        channel,
                        events: d.lost,
                    });
                }
                for e in d.events {
                    point.seq = e.seq;
                    emit(&Event::DataEvent {
                        channel,
                        seq: e.seq,
                        name: e.name,
                        data: e.data,
                        ts: e.ts,
                    });
                }
            }
            Err(ConsumeError::Busy) => owed.events = true,
            Err(ConsumeError::Read(e)) => return Err(e),
        }
    }
    Ok(())
}

/// Read nothing more from this producer: only wait for its `disconnect`.
fn wait_for_disconnect(c: &Consumer) {
    while c.wait(WAIT_MS) != Wake::Disconnect {}
}
