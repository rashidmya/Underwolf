//! Gate probe, helper side. `helper_probe <game windows pid>`, run under Wine in the prefix where
//! `game_probe` runs. It creates the objects for that pid (`Objects::create`, as the helper will),
//! then
//!   1. drives `ROUNDS` round trips: toggle `edit_mode` with `state::apply` under the lock, ring
//!      the changed bit, wait for `state_applied`, and read back `applied_seq == seq`;
//!   2. serves the lost-update check: swap `to_host`, count each sender's bit, acknowledge it.
//!      Every send must be counted exactly once;
//!   3. once `game_probe` rings `READY_BIT`, takes the lock, breaks the header's magic, rings
//!      `ABANDON_BIT` and exits without releasing the lock (the mutex is abandoned).
//!
//! Exit 0 and `ok` on stdout when every check passed; otherwise a nonzero code and the reason.

#[cfg(not(windows))]
fn main() {
    eprintln!("helper_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::Write;

    use uw_game_channel::objects::{Objects, LOCK_TIMEOUT_MS};
    use uw_pipe_proto::bells::{to_game, to_host};
    use uw_pipe_proto::state;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    /// Must match `game_probe`.
    const ROUNDS: u64 = 200;
    const SENDS: usize = 2000;
    const SENDER_BITS: [u64; 2] = [1 << 32, 1 << 33];
    const READY_BIT: u64 = 1 << 41;
    const ABANDON_BIT: u64 = 1 << 40;
    const WAIT_MS: u32 = 5000;

    /// Wait for `bit` on `bell-host`, OR-ing what each wake-up takes. A wake-up that takes nothing
    /// is a stale signal and is ignored: in phase 2 a sender's `SetEvent` can land after this probe
    /// already swapped its bit out, leaving the event signalled with an empty mask.
    fn wait_for_bit(o: &Objects, bit: u64) -> u64 {
        let mut seen = 0;
        for _ in 0..10 {
            // SAFETY: waits on the bell-host event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_host(), WAIT_MS) } != WAIT_OBJECT_0 {
                break;
            }
            seen |= o.take_to_host();
            if seen & bit != 0 {
                break;
            }
        }
        seen
    }

    pub fn main() -> i32 {
        let Some(pid) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
            eprintln!("usage: helper_probe <game windows pid>");
            return 2;
        };
        let o = match Objects::create(pid) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[helper_probe] create: {e}");
                return 3;
            }
        };

        // 1. Round trips.
        for round in 0..ROUNDS {
            let on = round % 2 == 0;
            let Ok(mut g) = o.lock(LOCK_TIMEOUT_MS) else {
                eprintln!("[helper_probe] round {round}: state lock busy");
                return 4;
            };
            let change = state::apply(g.bytes_mut(), Some(on), None);
            drop(g);
            let want = if on { to_game::EDIT_MODE_ON } else { to_game::EDIT_MODE_OFF };
            if change.seq != round + 1 || change.bits != want {
                eprintln!("[helper_probe] round {round}: change {change:?}");
                return 5;
            }
            o.ring_game(change.bits);
            // SAFETY: waits on the bell-host event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_host(), WAIT_MS) } != WAIT_OBJECT_0 {
                eprintln!("[helper_probe] round {round}: no bell-host");
                return 6;
            }
            let bits = o.take_to_host();
            if bits != to_host::STATE_APPLIED {
                eprintln!("[helper_probe] round {round}: bits {bits:#x}");
                return 7;
            }
            let Ok(g) = o.lock(LOCK_TIMEOUT_MS) else {
                eprintln!("[helper_probe] round {round}: state lock busy");
                return 8;
            };
            let s = state::read(g.bytes());
            if s.applied_seq != change.seq {
                eprintln!("[helper_probe] round {round}: applied_seq {} != seq {}", s.applied_seq, change.seq);
                return 9;
            }
        }

        // 2. Lost-update check.
        let mut counts = [0usize; 2];
        while counts.iter().any(|&n| n < SENDS) {
            // SAFETY: waits on the bell-host event handle `o` owns.
            if unsafe { WaitForSingleObject(o.bell_host(), WAIT_MS) } != WAIT_OBJECT_0 {
                eprintln!("[helper_probe] lost-update check stalled at {counts:?} of {SENDS}");
                return 10;
            }
            let bits = o.take_to_host();
            let mut ack = 0;
            for (i, bit) in SENDER_BITS.iter().enumerate() {
                if bits & bit != 0 {
                    counts[i] += 1;
                    ack |= bit;
                }
            }
            if bits & !(SENDER_BITS[0] | SENDER_BITS[1]) != 0 {
                eprintln!("[helper_probe] unexpected bits {bits:#x}");
                return 11;
            }
            if ack != 0 {
                o.ring_game(ack);
            }
        }

        // 3. Abandon the lock, with the header broken.
        if wait_for_bit(&o, READY_BIT) != READY_BIT {
            eprintln!("[helper_probe] phase 3: no ready bell");
            return 12;
        }
        let Ok(mut g) = o.lock(LOCK_TIMEOUT_MS) else {
            eprintln!("[helper_probe] phase 3: state lock busy");
            return 13;
        };
        g.bytes_mut()[0] = b'X';
        o.ring_game(ABANDON_BIT);
        println!("ok rounds={ROUNDS} sends={counts:?}");
        let _ = std::io::stdout().flush();
        // `exit` runs no destructors: `g` is never dropped, so the mutex is abandoned.
        std::process::exit(0)
    }
}
