//! Gate probe, game side. Run under Wine next to `helper_probe`, in the same prefix: it prints
//! `pid <windows pid>`, opens the objects the helper probe creates for that pid (`Objects::open`,
//! as the in-game worker will), then
//!   1. answers `ROUNDS` doorbell round trips: each `bell-game` must carry the round's bit, the
//!      state block (read under the lock) the helper's new `edit_mode` and `seq`; it writes
//!      `applied_seq` and rings `state_applied`;
//!   2. runs a lost-update check: two threads OR their own bit into `to_host` at the same time,
//!      each again only after the helper acknowledged the last one. A bit lost by a non-atomic OR
//!      or swap is never acknowledged, so its thread times out;
//!   3. checks that a second `lock` on the thread that holds a guard is `Busy` (the Win32 mutex is
//!      recursive), then rings `READY_BIT`;
//!   4. waits for the helper probe to exit while it holds the lock with a broken header: the next
//!      `lock` must get the abandoned mutex as acquired and re-check the header (`BadHeader`), and
//!      the one after it, a plain acquire, must succeed.
//!
//! Exit 0 and `ok` on stdout when every check passed; otherwise a nonzero code and the reason.

#[cfg(not(windows))]
fn main() {
    eprintln!("game_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use uw_game_channel::objects::{LockError, Objects, LOCK_TIMEOUT_MS};
    use uw_pipe_proto::bells::{to_game, to_host};
    use uw_pipe_proto::state::{self, HeaderError};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, WaitForSingleObject};

    /// Must match `helper_probe`.
    const ROUNDS: u64 = 200;
    const SENDS: usize = 2000;
    const SENDER_BITS: [u64; 2] = [1 << 32, 1 << 33];
    /// game -> helper: phase 3 is done. helper -> game: it holds the lock and is exiting.
    const READY_BIT: u64 = 1 << 41;
    const ABANDON_BIT: u64 = 1 << 40;
    const WAIT_MS: u32 = 5000;

    /// Wait for `bit` on `bell-game`, OR-ing what each wake-up takes. A wake-up that takes nothing
    /// is a stale signal and is ignored: in phase 2 a sender's `SetEvent` can land after the
    /// receiver already swapped its bit out, leaving the event signalled with an empty mask.
    fn wait_for_bit(o: &Objects, bit: u64) -> u64 {
        let mut seen = 0;
        for _ in 0..10 {
            // SAFETY: waits on the bell-game event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_game(), WAIT_MS) } != WAIT_OBJECT_0 {
                break;
            }
            seen |= o.take_to_game();
            if seen & bit != 0 {
                break;
            }
        }
        seen
    }

    pub fn main() -> i32 {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        println!("pid {pid}");
        let _ = std::io::stdout().flush();

        // The helper probe creates the objects once the test has read our pid.
        let start = Instant::now();
        let o = loop {
            match Objects::open(pid) {
                Ok(o) => break o,
                Err(_) if start.elapsed() < Duration::from_secs(60) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    eprintln!("[game_probe] open: {e}");
                    return 3;
                }
            }
        };

        // 1. Round trips.
        for round in 0..ROUNDS {
            let on = round % 2 == 0;
            // SAFETY: waits on the bell-game event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_game(), WAIT_MS) } != WAIT_OBJECT_0 {
                eprintln!("[game_probe] round {round}: no bell-game");
                return 4;
            }
            let bits = o.take_to_game();
            let want = if on { to_game::EDIT_MODE_ON } else { to_game::EDIT_MODE_OFF };
            if bits != want {
                eprintln!("[game_probe] round {round}: bits {bits:#x}, want {want:#x}");
                return 5;
            }
            let Ok(mut g) = o.lock(LOCK_TIMEOUT_MS) else {
                eprintln!("[game_probe] round {round}: state lock busy");
                return 6;
            };
            let s = state::read(g.bytes());
            if s.seq != round + 1 || s.edit_mode != on {
                eprintln!("[game_probe] round {round}: state {s:?}");
                return 7;
            }
            state::set_applied_seq(g.bytes_mut(), s.seq);
            drop(g);
            o.ring_host(to_host::STATE_APPLIED);
        }

        // 2. Lost-update check. One dispatcher takes `to_game` (the helper's acknowledgements)
        // and hands each sender its own; the senders ring concurrently.
        let o = Arc::new(o);
        let done = Arc::new(AtomicUsize::new(0));
        let mut acks: Vec<Sender<()>> = Vec::new();
        let mut senders = Vec::new();
        for bit in SENDER_BITS {
            let (tx, rx) = channel::<()>();
            acks.push(tx);
            let (o, done) = (Arc::clone(&o), Arc::clone(&done));
            senders.push(std::thread::spawn(move || {
                for i in 0..SENDS {
                    o.ring_host(bit);
                    if rx.recv_timeout(Duration::from_millis(WAIT_MS as u64)).is_err() {
                        done.fetch_add(1, Ordering::SeqCst);
                        return Err(format!("bit {bit:#x}: send {i} was never acknowledged (lost)"));
                    }
                }
                done.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }));
        }
        while done.load(Ordering::SeqCst) < SENDER_BITS.len() {
            // SAFETY: waits on the bell-game event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_game(), 100) } != WAIT_OBJECT_0 {
                continue;
            }
            let bits = o.take_to_game();
            for (i, bit) in SENDER_BITS.iter().enumerate() {
                if bits & bit != 0 {
                    let _ = acks[i].send(());
                }
            }
        }
        for s in senders {
            if let Err(e) = s.join().unwrap() {
                eprintln!("[game_probe] {e}");
                return 8;
            }
        }

        // 3. One guard at a time per thread.
        let Ok(g) = o.lock(LOCK_TIMEOUT_MS) else {
            eprintln!("[game_probe] phase 3: state lock busy");
            return 9;
        };
        if !matches!(o.lock(0), Err(LockError::Busy)) {
            eprintln!("[game_probe] a second lock on the same thread was granted");
            return 10;
        }
        drop(g);
        if o.lock(0).is_err() {
            eprintln!("[game_probe] the lock was not free after the guard dropped");
            return 11;
        }
        o.ring_host(READY_BIT);

        // 4. The helper probe exits holding the lock, with the header broken.
        if wait_for_bit(&o, ABANDON_BIT) != ABANDON_BIT {
            eprintln!("[game_probe] phase 4: no abandon bell");
            return 12;
        }
        match o.lock(WAIT_MS) {
            Err(LockError::BadHeader(HeaderError::BadMagic)) => {}
            other => {
                eprintln!("[game_probe] abandoned lock: {:?}, want BadHeader(BadMagic)", other.map(|_| ()));
                return 13;
            }
        }
        // That lock released it again; a plain acquire does not re-check the header.
        match o.lock(LOCK_TIMEOUT_MS) {
            Ok(g) if state::check_header(g.bytes()) == Err(HeaderError::BadMagic) => {}
            other => {
                eprintln!("[game_probe] lock after the abandon: {:?}", other.map(|_| ()));
                return 14;
            }
        }
        println!("ok");
        0
    }
}
