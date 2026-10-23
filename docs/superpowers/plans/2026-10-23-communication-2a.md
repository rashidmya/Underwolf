# Communication, Control Channel (Milestone 1, Plan 2a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Linux host and the in-game overlay exchange commands, doorbells and shared state through Windows IPC that the Wine-side helper owns and relays (design `docs/superpowers/specs/2026-10-23-communication-2a-design.md`, spec §6.3, control half).

**Architecture:** `uw-pipe-proto` grows the overlay pipe's messages, the doorbell bits, the state block layout v1 and JSON lines with request ids. A new `uw-game-channel` crate holds the per-game named objects (two auto-reset doorbell events, a 16-byte mask region with lock-free atomics, the state block and its mutex): the helper creates them, the game only opens them, and the crate's worker thread is what an overlay module starts. The helper creates the channel and the overlay pipe before its bootstrap pipe and hook, reads stdin from the start (stdin closing ends it in every phase), and after the bootstrap's `hello` runs one wait loop over commands, `bell-host` and both pipes. A new Linux library `uw-host` takes discovery from `uw-inject` and adds a pure supervisor (request ids, restarts with back-off, stop rules, the overlay timeout, reconcile of a desired state) plus a runtime that drives real helper processes; `uw-inject` becomes a thin CLI on it.

**Tech Stack:** Rust (edition 2024, rustc 1.96.1). Targets `x86_64-unknown-linux-gnu` (host crates, all unit tests) and `x86_64-pc-windows-gnu` (helper, bootstrap, `uw-game-channel`, test overlay, test game). `windows-sys = "0.61.2"`. `serde`/`serde_json`, `libc`. Wine tests: system Wine 11.20 (`/usr/bin/wine`) under `xvfb-run -a`, `--test-threads=1`, each run bounded by `timeout`.

**Base:** `main` at `2673388`: `crates/` as plan 1 left it (`c683b7d`, with its final-review fixes) and the design with the owner's decisions after the plan review (a `ping` timeout, `overlay_not_responding`; `hook_failed` and `bootstrap_timeout` stop `uw-host`). Every snippet below was written against the code in `crates/` now, and every task was built and run in a scratch clone before this plan was written: unit tests and host and Windows clippy at every task, and the Wine suites (gate 1, helper 14, `uw-inject` 2), twice on the final tree, with no processes or prefixes left behind. The counts in the expected output come from that run.

---

## Why these facts (looked up 2026-10-23)

- **`windows-sys` 0.61.2 names**, read in `~/.cargo/registry/src/*/windows-sys-0.61.2/src/Windows/Win32/`:
  - `System/Threading`: `CreateEventW`, `OpenEventW`, `SetEvent`, `CreateMutexW`, `OpenMutexW`, `ReleaseMutex`, `WaitForSingleObject`, `WaitForMultipleObjects`; `EVENT_MODIFY_STATE = 2`, `MUTEX_MODIFY_STATE = 1`, `SYNCHRONIZATION_SYNCHRONIZE = 0x100000`. `CreateEventW` and `CreateMutexW` are gated on `Win32_Security`.
  - `System/Memory`: `CreateFileMappingW` (gated on `Win32_Security`), `OpenFileMappingW`, `MapViewOfFile` (returns `MEMORY_MAPPED_VIEW_ADDRESS { Value: *mut c_void }`), `UnmapViewOfFile` (takes that struct), `VirtualQuery`, `MEMORY_BASIC_INFORMATION` (`RegionSize`); `FILE_MAP_READ = 4`, `FILE_MAP_WRITE = 2`, `PAGE_READWRITE = 4`.
  - `Foundation`: `WAIT_OBJECT_0 = 0`, `WAIT_ABANDONED = 0x80`, `WAIT_TIMEOUT = 258`, `ERROR_ALREADY_EXISTS = 183`, `ERROR_IO_INCOMPLETE`, `ERROR_IO_PENDING`, `ERROR_PIPE_CONNECTED`.
  Plan 1's names (pipes, Toolhelp, hooks) are unchanged.
- **`AtomicU64::from_ptr`** is stable since Rust 1.75, so the mask words are plain `AtomicU64`s on the mapped view (offsets 0 and 8 of a page-aligned view are 8-byte aligned).
- **Cross-process atomics on a Wine mapping** (the design's open risk) are what Task 1 proves. In the planner's scratch run under Wine 11.20 the gate passed: 200 doorbell round trips and 2 × 2000 lost-update sends, with no loss. Making the masks non-atomic (load, yield, store) made it fail at once (`lost-update check stalled`). Task 1 still runs it first, as the gate.
- **Wine reports an abandoned mutex.** A process that exits holding the state lock leaves it abandoned: the next waiter gets `WAIT_ABANDONED` (the gate's last phase checks this; with the header re-check removed it fails). Win32 mutexes are recursive, so `Objects::lock` keeps a "held" flag to hand out one guard at a time.
- **`ERROR_MORE_DATA` (234)** is what a message-mode read returns for a message larger than the buffer; the pipe server reports it as an oversized message (`pipe_error`).
- **`read -r`** in the fake-helper `sh` scripts (Task 4): plain `read` turns `\\` into `\` and would mangle the JSON.

## File map

| Path | Builds | Responsibility |
|---|---|---|
| `crates/uw-pipe-proto/src/names.rs` | lib (both) | + overlay pipe, doorbell, mask, state and state-lock names |
| `crates/uw-pipe-proto/src/bells.rs` | new | Mask region layout, v1 bits, `bit_names` (`bit<N>` for unknown bits) |
| `crates/uw-pipe-proto/src/state.rs` | new | State block layout v1 on byte slices: header init/check, field read/write, `apply` (`set_state`) |
| `crates/uw-pipe-proto/src/pipe.rs` | lib | + `OverlayMessage` (types 0x0101-0x0103), `encode_overlay`/`decode_overlay`, `check_overlay_first` |
| `crates/uw-pipe-proto/src/jsonl.rs` | lib | Commands and events with `request_id`; `set_state`, `ping`; overlay, bell and disconnect events; `uw-host`'s events |
| `crates/uw-pipe-proto/tests/{state,bells,pipe,jsonl}.rs` | tests | Layouts, rejections, wire names |
| `crates/uw-game-channel/src/objects.rs` | new, Windows | The named objects: `Objects::create` (helper), `Objects::open` (game), ring/take, `lock` with `WAIT_ABANDONED` handling |
| `crates/uw-game-channel/src/worker.rs` | new, Windows | The in-game worker: overlay pipe client, hello, objects, bells → callback, ping → pong |
| `crates/uw-game-channel/examples/{game,helper}_probe.rs` | new, Windows | The gate's two probe programs |
| `crates/uw-game-channel/tests/wine_objects.rs` | new, ignored | The gate: both probes in one Wine prefix |
| `crates/uw-helper/src/decide.rs` | lib | + `create_channel` before `create_pipe`, `ChannelError`, `Outcome::ChannelFailed`, event mapping changes |
| `crates/uw-helper/src/pending.rs` | new | Requests awaiting their reply, with deadlines: `load_overlay` (`status`) and `ping` (`pong`); late replies discarded |
| `crates/uw-helper/src/bin/uw-helper64/main.rs` | bin, Windows | Arguments, stdin reader start, `decide::run`, hand-over to the relay |
| `crates/uw-helper/src/bin/uw-helper64/win32.rs` | bin, Windows | The real `Win32` (moved from plan 1's single file) + `Channel` (objects and overlay pipe) |
| `crates/uw-helper/src/bin/uw-helper64/pipe_server.rs` | bin, Windows | One overlapped pipe server for both pipes (`poll`, `accept_hello`, `write`) |
| `crates/uw-helper/src/bin/uw-helper64/commands.rs` | new, Windows | The stdin reader thread; stdin closing exits the helper |
| `crates/uw-helper/src/bin/uw-helper64/relay.rs` | bin, Windows | The one wait loop; the game watcher |
| `crates/uw-helper/tests/{decide,pending,wine_e2e}.rs` | tests | Fake-`Win32` logic; pending; the helper-level Wine scenarios |
| `crates/uw-host/src/{lib,discovery,driver,supervisor,host}.rs` | new, Linux | Discovery (moved), desired state and reconcile, the supervisor state machine, the process runtime |
| `crates/uw-host/tests/{cli,discovery,driver,supervisor,host}.rs` | tests | Moved discovery tests + Task 3 N1, the supervisor against a scripted fake helper, the runtime against `sh` fake helpers |
| `crates/uw-inject/src/bin/uw-inject.rs` | bin, Linux | Thin CLI on `uw-host` (its lib, `discovery.rs` and `driver.rs` move out); maps user request ids to the host's and back (`UserIds`, unit-tested in the bin) |
| `crates/uw-inject/tests/wine_e2e.rs` | test, ignored | Plan 1's chain test + the helper-restart scenario |
| `crates/uw-test-overlay/src/lib.rs` | test DLL | Starts the channel; writes `uw-test-state-<pid>.txt` and acknowledges each bell; optional slow start |
| `crates/uw-test-game/src/main.rs` | test exe | + `--bad-state-block` |

## Decisions the design leaves open (reviewers: check these)

- **One objects module, two constructors.** The objects' Win32 code lives once, in `uw-game-channel::objects` (as Spike 3's producer and consumer shared `gep-channel/src/win.rs`): `Objects::create` is called only by the helper, `Objects::open` only by the game. The helper's "objects" job (design code layout) is `win32::Channel`, which creates and holds them. Both helper pipes use one `PipeServer` (`pipe_server.rs`).
- **Error codes.** Beyond the design's first draft: `bootstrap_not_connected` (`load_overlay` with no bootstrap, the twin of `overlay_not_connected`); `channel_failed` (an object or the overlay pipe could not be created; helper-local like plan 1's `pipe_error`); `helper_start_failed` (`uw-host` could not spawn the helper; stops the host); and the owner's `overlay_not_responding`. The design's error-code list now names them all. A command still pending when its helper dies is answered with the same `*_not_connected` code, and a `ping` without its `pong` in time with `overlay_not_responding`, so every command gets exactly one reply.
- **Commands before the connect are refused, not queued.** The stdin reader runs from the start, so commands can arrive while the helper still hooks; right after `decide::run` returns, before `bootstrap_connected` goes out, the helper answers every queued command (`bootstrap_not_connected`, `overlay_not_connected`). `uw-host`'s reconcile commands, sent in answer to `bootstrap_connected`, are not affected.
- **User request ids are mapped.** `uw-inject` sends every command with an id from the host's counter and puts the user's own id back on the reply, so a user's id can never collide with one the host took for its reconcile commands.
- **Desired state default:** overlay visible, not in edit mode (`Desired::new`). The first `overlay_connected` therefore rings `show`.
- **`set_state` needs the overlay** (it is one of "the commands that need the overlay"): with none connected it fails with `overlay_not_connected`, and the desired state is pushed on the next `overlay_connected`.
- **A bell is not a command.** If the state lock is busy when a bell is read (helper or worker), the bits are put back and the bell rung again, so the next pass retries; only `set_state` fails with `state_busy`.
- **The worker keeps the objects and `seen_seq` across reconnects** and drops the old objects only after the new ones are open, so the game holds the state block across a helper restart (the design's `ERROR_ALREADY_EXISTS` path) and `seq` keeps counting.
- **`uw-inject`'s stdin closing does not stop it** (plan 1 usage without stdin keeps working); SIGINT/SIGTERM `_exit` at once and the helper exits on its own because its stdin closes (no kill).
- **Test aids:** the gate probes (examples), `UW_STATUS_TIMEOUT_MS` and `UW_PING_TIMEOUT_MS` (helper, like plan 1's `UW_*_TIMEOUT_MS`), `UW_TEST_OVERLAY_START_DELAY_MS`, `UW_TEST_OVERLAY_BELL_DELAY_MS` and the state file (test overlay). The state file's stem `uw-test-state-` is not `uw-test-overlay-`, so plan 1's single-marker checks still hold.

## Conventions for every task

- Work only in the repo; never run anything against Deadlock or Steam (Task 12 is run by the controller).
- Wine runs: always `timeout … xvfb-run -a cargo test … -- --ignored --test-threads=1`, Windows artifacts built first. Afterwards this prints nothing: `ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep`, and no `/tmp/uw-inject-*` directory is left.
- Every task touching Windows code runs `cargo clippy --target x86_64-pc-windows-gnu -p <crate> --all-targets -- -D warnings` (host clippy cannot see `cfg(windows)` code) and the host clippy.
- Commits: the repo's git user, the message given, no `Co-Authored-By`, no AI or Claude mention, no session links. `Cargo.lock` is committed when it changes.

---

## Task 1: Gate: the channel objects between two Wine processes

The design's open risk ("Lock-free atomics on a Wine shared mapping across processes … not yet proven"). This task builds the real pieces minimally and proves them under plain Wine: the object names, the mask region and state block layouts (`uw-pipe-proto`), and the objects module (`uw-game-channel::objects`, `create` for the helper and `open` for the game). Two probe programs, examples in `uw-game-channel`, use the real module from two Wine processes in one prefix, as the helper and the game will: doorbell round trips through the state block, a lost-update check on the mask, the one-guard-per-thread rule of `Objects::lock` (the Win32 mutex is recursive, so a "held" flag returns `Busy` for a second guard), and an abandoned lock (the helper probe exits holding it with a broken header: the game probe must get it as acquired and see the header re-checked, design "Failures"). The last two phases wait for their bit in a bounded loop that ignores a wake-up with an empty mask: phase 2 can leave a doorbell signalled after its bit was already taken (a sender's `SetEvent` lands after the receiver's swap), as the real relay and worker also allow (both skip an empty take). Nothing here is thrown away: the layouts and objects are what Tasks 6-9 use, and the probes stay as a Wine test. **If the gate fails, stop and report to the owner.**

**Files:**
- Modify: `crates/uw-pipe-proto/src/lib.rs`, `crates/uw-pipe-proto/src/names.rs`
- Create: `crates/uw-pipe-proto/src/bells.rs`, `crates/uw-pipe-proto/src/state.rs`, `crates/uw-pipe-proto/tests/state.rs`
- Create: `crates/uw-game-channel/Cargo.toml`, `crates/uw-game-channel/src/lib.rs`, `crates/uw-game-channel/src/objects.rs`
- Create: `crates/uw-game-channel/examples/game_probe.rs`, `crates/uw-game-channel/examples/helper_probe.rs`, `crates/uw-game-channel/tests/wine_objects.rs`

- [ ] **Step 1: Write the failing layout tests**

`crates/uw-pipe-proto/tests/state.rs` (complete file):

```rust
use uw_pipe_proto::bells::to_game;
use uw_pipe_proto::state::{
    apply, check_header, init_header, read, set_applied_seq, Change, HeaderError, StateV1, BLOCK_SIZE,
    LAYOUT_VERSION,
};

fn fresh() -> Vec<u8> {
    let mut b = vec![0u8; BLOCK_SIZE];
    init_header(&mut b);
    b
}

/// Catches a header written at other offsets than the design's table (magic 0, version 4, size 8).
#[test]
fn the_header_sits_at_the_design_offsets() {
    let b = fresh();
    assert_eq!(&b[0..4], b"UWS1");
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), 1);
    assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 64);
    assert_eq!(BLOCK_SIZE, 64);
    assert_eq!(LAYOUT_VERSION, 1);
    assert_eq!(check_header(&b), Ok(()));
}

/// Catches fields read from or written to the wrong offsets (seq 16, applied_seq 24, edit_mode 32,
/// visible 33), which the two sides would then disagree on.
#[test]
fn fields_sit_at_the_design_offsets() {
    let mut b = fresh();
    b[16..24].copy_from_slice(&7u64.to_le_bytes());
    b[32] = 1;
    b[33] = 1;
    set_applied_seq(&mut b, 5);
    assert_eq!(u64::from_le_bytes(b[24..32].try_into().unwrap()), 5);
    assert_eq!(read(&b), StateV1 { seq: 7, applied_seq: 5, edit_mode: true, visible: true });
}

/// Catches a header check that accepts a block of another layout (design: `version_mismatch`).
#[test]
fn a_foreign_header_is_rejected() {
    let mut b = fresh();
    b[0] = b'X';
    assert_eq!(check_header(&b), Err(HeaderError::BadMagic));

    let mut b = fresh();
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(check_header(&b), Err(HeaderError::BadVersion(2)));

    let mut b = fresh();
    b[8..12].copy_from_slice(&128u32.to_le_bytes());
    assert_eq!(check_header(&b), Err(HeaderError::BadSize(128)));

    assert_eq!(check_header(&[0u8; 16]), Err(HeaderError::Short(16)));
    // A zeroed block (created but never initialised) is not v1 either.
    assert_eq!(check_header(&[0u8; BLOCK_SIZE]), Err(HeaderError::BadMagic));
}

/// Catches `set_state` bumping `seq` (and ringing) when nothing changed, or naming the wrong bits.
#[test]
fn apply_bumps_seq_once_per_change_and_names_the_changed_fields() {
    let mut b = fresh();
    assert_eq!(apply(&mut b, Some(true), None), Change { seq: 1, bits: to_game::EDIT_MODE_ON });
    assert_eq!(apply(&mut b, Some(true), Some(true)), Change { seq: 2, bits: to_game::SHOW });
    assert_eq!(
        apply(&mut b, Some(false), Some(false)),
        Change { seq: 3, bits: to_game::EDIT_MODE_OFF | to_game::HIDE }
    );
    assert_eq!(read(&b), StateV1 { seq: 3, applied_seq: 0, edit_mode: false, visible: false });
}

#[test]
fn apply_without_a_change_keeps_seq_and_rings_nothing() {
    let mut b = fresh();
    apply(&mut b, Some(true), Some(true));
    assert_eq!(apply(&mut b, Some(true), Some(true)), Change { seq: 1, bits: 0 });
    assert_eq!(apply(&mut b, None, None), Change { seq: 1, bits: 0 });
    assert_eq!(read(&b).seq, 1);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test state`
Expected: does not compile: `error[E0432]: unresolved import` for `uw_pipe_proto::bells` and `uw_pipe_proto::state`.

- [ ] **Step 3: Add the names, the mask region and the state block layout**

`crates/uw-pipe-proto/src/names.rs` (complete file):

```rust
//! Per-game object names. `<pid>` is the game's Windows (Wine) process id in every name (design
//! §3), so the helper (which knows the pid from Toolhelp) and the bootstrap (which reads its own
//! `GetCurrentProcessId`) build the same strings. Events, mutexes and file mappings share one
//! namespace per session (a name taken by another kind fails), so each kind has its own suffix.

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
pub fn helper_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-helper-{pid}")
}
pub fn marker_mutex_name(pid: u32) -> String {
    format!(r"Local\underwolf-bootstrap-{pid}")
}
/// Host -> game doorbell (auto-reset event).
pub fn bell_game_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bell-game")
}
/// Game -> host doorbell (auto-reset event).
pub fn bell_host_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bell-host")
}
/// The 16-byte doorbell mask region (file mapping).
pub fn bells_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-bells")
}
/// The state block (file mapping).
pub fn state_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-state")
}
/// The state block's lock (mutex).
pub fn state_lock_name(pid: u32) -> String {
    format!(r"Local\underwolf-{pid}-state-lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
        assert_eq!(bell_game_name(1234), r"Local\underwolf-1234-bell-game");
        assert_eq!(bell_host_name(1234), r"Local\underwolf-1234-bell-host");
        assert_eq!(bells_name(1234), r"Local\underwolf-1234-bells");
        assert_eq!(state_name(1234), r"Local\underwolf-1234-state");
        assert_eq!(state_lock_name(1234), r"Local\underwolf-1234-state-lock");
    }

    /// Catches two kinds of kernel object sharing a name: the second `Create*W` would then fail
    /// with `ERROR_INVALID_HANDLE` (design "Platform facts").
    #[test]
    fn kernel_object_names_are_all_different() {
        let names = [
            helper_mutex_name(7),
            marker_mutex_name(7),
            bell_game_name(7),
            bell_host_name(7),
            bells_name(7),
            state_name(7),
            state_lock_name(7),
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
```

`crates/uw-pipe-proto/src/bells.rs` (complete file):

```rust
//! Doorbell bits v1 and the mask region (design "Messages and layouts"). Two auto-reset events
//! ring the other side; the 16-byte region `Local\underwolf-<pid>-bells` says why: `to_game` (u64
//! at offset 0) and `to_host` (u64 at offset 8). Senders OR bits in with an atomic operation, then
//! signal; receivers swap the mask to 0. A receiver ignores bits it does not know, so later plans
//! add bits without a version change.

/// Size of the mask region in bytes.
pub const MASK_REGION_SIZE: usize = 16;
/// Offset of the host -> game mask.
pub const TO_GAME_OFFSET: usize = 0;
/// Offset of the game -> host mask.
pub const TO_HOST_OFFSET: usize = 8;

/// Host -> game bits.
pub mod to_game {
    pub const EDIT_MODE_ON: u64 = 1 << 0;
    pub const EDIT_MODE_OFF: u64 = 1 << 1;
    pub const SHOW: u64 = 1 << 2;
    pub const HIDE: u64 = 1 << 3;
}

/// Game -> host bits.
pub mod to_host {
    pub const STATE_APPLIED: u64 = 1 << 0;
}
```

`crates/uw-pipe-proto/src/state.rs` (complete file):

```rust
//! The state block, layout v1 (design "State block layout v1"): `Local\underwolf-<pid>-state`,
//! fixed little-endian offsets, no pointers. The helper creates the block and writes its header
//! once; after that both sides read and write the fields only while holding
//! `Local\underwolf-<pid>-state-lock`. Everything here works on the mapped bytes as a plain slice,
//! so it is unit-tested on Linux.
//!
//! | Offset | Field | Type | Writer |
//! |---|---|---|---|
//! | 0 | magic `"UWS1"` | [u8; 4] | helper, at creation |
//! | 4 | layout version (1) | u16 | helper, at creation |
//! | 8 | size of the block in bytes | u32 | helper, at creation |
//! | 16 | `seq` | u64 | helper |
//! | 24 | `applied_seq` | u64 | overlay |
//! | 32 | `edit_mode` (0/1) | u8 | helper |
//! | 33 | `visible` (0/1) | u8 | helper |

use crate::bells::to_game;

pub const MAGIC: [u8; 4] = *b"UWS1";
pub const LAYOUT_VERSION: u16 = 1;
/// Bytes in a layout v1 block: the header's `size` field, and the size the helper maps.
pub const BLOCK_SIZE: usize = 64;

const OFF_VERSION: usize = 4;
const OFF_SIZE: usize = 8;
const OFF_SEQ: usize = 16;
const OFF_APPLIED_SEQ: usize = 24;
const OFF_EDIT_MODE: usize = 32;
const OFF_VISIBLE: usize = 33;

/// The fields of a layout v1 block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateV1 {
    pub seq: u64,
    pub applied_seq: u64,
    pub edit_mode: bool,
    pub visible: bool,
}

/// Why a block's header is not layout v1. The helper refuses such a block with
/// `version_mismatch` ("restart the game") and never rewrites it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    /// The mapped bytes are fewer than `BLOCK_SIZE` (carries the length seen).
    Short(usize),
    BadMagic,
    BadVersion(u16),
    BadSize(u32),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderError::Short(n) => write!(f, "state block is {n} bytes, under {BLOCK_SIZE}"),
            HeaderError::BadMagic => write!(f, "state block magic is not UWS1"),
            HeaderError::BadVersion(v) => write!(f, "state block layout {v}, not {LAYOUT_VERSION}"),
            HeaderError::BadSize(s) => write!(f, "state block size field {s}, not {BLOCK_SIZE}"),
        }
    }
}
impl std::error::Error for HeaderError {}

/// The outcome of a `set_state`: `seq` after it, and the `to_game` bits of the fields that
/// changed. `bits == 0` means nothing changed and `seq` was not bumped (so no bell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub bits: u64,
}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Write the v1 header into a block the helper just created (all zero). Panics if `b` is shorter
/// than `BLOCK_SIZE`.
pub fn init_header(b: &mut [u8]) {
    b[..4].copy_from_slice(&MAGIC);
    b[OFF_VERSION..OFF_VERSION + 2].copy_from_slice(&LAYOUT_VERSION.to_le_bytes());
    b[OFF_SIZE..OFF_SIZE + 4].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
}

/// Check magic, layout version and size (design: a mismatch is `version_mismatch`).
pub fn check_header(b: &[u8]) -> Result<(), HeaderError> {
    if b.len() < BLOCK_SIZE {
        return Err(HeaderError::Short(b.len()));
    }
    if b[..4] != MAGIC {
        return Err(HeaderError::BadMagic);
    }
    let version = rd_u16(b, OFF_VERSION);
    if version != LAYOUT_VERSION {
        return Err(HeaderError::BadVersion(version));
    }
    let size = rd_u32(b, OFF_SIZE);
    if size as usize != BLOCK_SIZE {
        return Err(HeaderError::BadSize(size));
    }
    Ok(())
}

/// Read the fields. Panics if `b` is shorter than `BLOCK_SIZE`.
pub fn read(b: &[u8]) -> StateV1 {
    StateV1 {
        seq: rd_u64(b, OFF_SEQ),
        applied_seq: rd_u64(b, OFF_APPLIED_SEQ),
        edit_mode: b[OFF_EDIT_MODE] != 0,
        visible: b[OFF_VISIBLE] != 0,
    }
}

/// The overlay records the last `seq` it applied.
pub fn set_applied_seq(b: &mut [u8], seq: u64) {
    b[OFF_APPLIED_SEQ..OFF_APPLIED_SEQ + 8].copy_from_slice(&seq.to_le_bytes());
}

/// The helper's `set_state`: write the given fields that differ, and bump `seq` once if any did.
pub fn apply(b: &mut [u8], edit_mode: Option<bool>, visible: Option<bool>) -> Change {
    let mut bits = 0;
    if let Some(on) = edit_mode
        && (b[OFF_EDIT_MODE] != 0) != on
    {
        b[OFF_EDIT_MODE] = on as u8;
        bits |= if on { to_game::EDIT_MODE_ON } else { to_game::EDIT_MODE_OFF };
    }
    if let Some(on) = visible
        && (b[OFF_VISIBLE] != 0) != on
    {
        b[OFF_VISIBLE] = on as u8;
        bits |= if on { to_game::SHOW } else { to_game::HIDE };
    }
    let mut seq = rd_u64(b, OFF_SEQ);
    if bits != 0 {
        seq = seq.wrapping_add(1);
        b[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&seq.to_le_bytes());
    }
    Change { seq, bits }
}
```

`crates/uw-pipe-proto/src/lib.rs` (complete file):

```rust
//! Underwolf injection protocols. `pipe` is the fixed-layout binary channel between the Wine-side
//! helper and the in-game bootstrap (spec 6.3); `jsonl` is the temporary one-object-per-line
//! channel between the Linux host and the helper (plan 2 replaces it with a binary relay);
//! `names` is the per-game pipe and mutex names both sides must agree on. `bells` (doorbell bits
//! and the mask region) and `state` (the state block, layout v1) are the control channel's shared
//! memory layouts.

pub mod bells;
pub mod jsonl;
pub mod names;
pub mod pipe;
pub mod state;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p uw-pipe-proto`
Expected: all pass: unit tests (names) 2, `jsonl` 5, `pipe` 14, `state` 5.

- [ ] **Step 5: Create `uw-game-channel` with the objects module**

The workspace already has `members = ["spikes/*", "crates/*"]`, so the new crate joins it by existing.

`crates/uw-game-channel/Cargo.toml` (complete file):

```toml
[package]
name = "uw-game-channel"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The control channel's in-game side (design 2a). `objects` holds the named doorbells, mask
# region, state block and lock: the helper creates them (`Objects::create`) and the game only
# opens them (`Objects::open`). Windows only; on Linux the crate is empty. The examples are the
# gate probes run by `tests/wine_objects.rs`.
[lib]
name = "uw_game_channel"

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(windows)'.dependencies]
# `CreateEventW`, `CreateMutexW` and `CreateFileMappingW` take `SECURITY_ATTRIBUTES`, so
# `Win32_Security` is needed even though they get null.
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_Memory",
    "Win32_System_Threading",
] }
```

`crates/uw-game-channel/src/lib.rs` (complete file):

```rust
//! The control channel between the helper and the in-game overlay (design 2a). `objects` holds
//! the per-game named objects: two auto-reset doorbell events, the 16-byte mask region, the state
//! block and its lock. The helper creates them before its pipes; the game only opens them, after
//! its `overlay_hello`. Windows only.

#[cfg(windows)]
pub mod objects;
```

`crates/uw-game-channel/src/objects.rs` (complete file):

```rust
//! The channel's named kernel objects for one game (`<pid>` = the game's Windows pid, names from
//! `uw_pipe_proto::names`): the `bell-game` and `bell-host` auto-reset events, the 16-byte mask
//! region, the state block and its lock. The helper creates them (`Objects::create`, before its
//! pipes); the game only opens them (`Objects::open`, after its `overlay_hello`).
//!
//! The masks are atomics on the shared view: a sender ORs bits in and signals the event, a
//! receiver swaps the mask to 0, so ringing never waits. The state block is read and written only
//! through `StateGuard`, which holds the state lock.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use uw_pipe_proto::bells::{MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET};
use uw_pipe_proto::names::{bell_game_name, bell_host_name, bells_name, state_lock_name, state_name};
use uw_pipe_proto::state::{self, HeaderError, BLOCK_SIZE};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, ReleaseMutex, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, MUTEX_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};

/// How long the helper and the game's worker wait for the state lock (design: 100 ms, then the
/// command fails with `state_busy`). Game threads other than the worker use 0.
pub const LOCK_TIMEOUT_MS: u32 = 100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLastError() }
}

/// Why the objects could not be created or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectsError {
    /// The state block's header is not layout v1. For the helper: a block the game held across a
    /// helper restart, from another client version (design: `version_mismatch`, "restart the
    /// game"; it is never rewritten).
    VersionMismatch(HeaderError),
    /// The state lock was not acquired within `LOCK_TIMEOUT_MS`.
    Busy,
    /// A Win32 call failed: which call on which object, and its `GetLastError`.
    Win32 { what: String, code: u32 },
}

impl std::fmt::Display for ObjectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectsError::VersionMismatch(h) => write!(f, "{h}"),
            ObjectsError::Busy => write!(f, "state lock busy"),
            ObjectsError::Win32 { what, code } => write!(f, "{what}: error {code}"),
        }
    }
}
impl std::error::Error for ObjectsError {}

fn win32(call: &str, name: &str) -> ObjectsError {
    ObjectsError::Win32 { what: format!("{call} {name}"), code: last_error() }
}

/// Why `Objects::lock` did not return a guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    /// Not acquired within the timeout (design: `state_busy`), or this thread already holds a
    /// `StateGuard` from the same `Objects`.
    Busy,
    /// Acquired from an owner that died holding it (`WAIT_ABANDONED`), and the header no longer
    /// checks out. The lock is released again.
    BadHeader(HeaderError),
}

impl From<LockError> for ObjectsError {
    fn from(e: LockError) -> ObjectsError {
        match e {
            LockError::Busy => ObjectsError::Busy,
            LockError::BadHeader(h) => ObjectsError::VersionMismatch(h),
        }
    }
}

/// An owned kernel handle, closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: closes the handle this value owns, exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A mapped view of a whole file mapping, unmapped on drop. `len` is the mapped region from
/// `VirtualQuery` (whole pages, so at least the object's size).
struct View {
    ptr: *mut u8,
    len: usize,
}

impl View {
    fn map(mapping: &Handle, name: &str) -> Result<View, ObjectsError> {
        // SAFETY: maps the whole object read/write; `VirtualQuery` only describes our own view.
        unsafe {
            let addr = MapViewOfFile(mapping.0, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
            if addr.Value.is_null() {
                return Err(win32("MapViewOfFile", name));
            }
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(addr.Value, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
                let e = win32("VirtualQuery", name);
                UnmapViewOfFile(addr);
                return Err(e);
            }
            Ok(View { ptr: addr.Value.cast(), len: info.RegionSize })
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: unmaps the view this value owns, exactly once.
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr.cast() }) };
    }
}

/// One game's channel objects, as the helper created or the game opened them.
pub struct Objects {
    bell_game: Handle,
    bell_host: Handle,
    _bells_mapping: Handle,
    bells: View,
    lock: Handle,
    _state_mapping: Handle,
    state: View,
    /// A `StateGuard` is alive. The Win32 mutex is recursive, so without this a second `lock` on
    /// the thread that holds it would hand out a second guard (two aliasing `bytes_mut`).
    held: AtomicBool,
}

// SAFETY: kernel handles may be used from any thread; the masks are only touched through
// atomics, and the state block only through a `StateGuard`, which holds the cross-process lock.
unsafe impl Send for Objects {}
// SAFETY: as above; `&Objects` hands out no unsynchronised access.
unsafe impl Sync for Objects {}

fn create_event(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: an auto-reset (bManualReset = FALSE), initially unsignaled, named event. An event
    // that already exists is returned as is (design "Platform facts").
    let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("CreateEventW", name)) } else { Ok(Handle(h)) }
}

fn create_mutex(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: a named mutex, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) };
    if h.is_null() { Err(win32("CreateMutexW", name)) } else { Ok(Handle(h)) }
}

/// Create (or open, if it exists) a paging-file mapping of `size` bytes. Returns whether it
/// already existed: then it has its own current size and contents (design "Platform facts").
fn create_mapping(name: &str, size: usize) -> Result<(Handle, View, bool), ObjectsError> {
    let w = wide(name);
    // SAFETY: a named, paging-file-backed, read/write mapping; `GetLastError` is read right after.
    let (h, existed) = unsafe {
        let h = CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, size as u32, w.as_ptr());
        (h, GetLastError() == ERROR_ALREADY_EXISTS)
    };
    if h.is_null() {
        return Err(win32("CreateFileMappingW", name));
    }
    let h = Handle(h);
    let view = View::map(&h, name)?;
    if view.len < size {
        return Err(ObjectsError::Win32 { what: format!("{name} maps {} bytes, under {size}", view.len), code: 0 });
    }
    Ok((h, view, existed))
}

fn open_event(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named event to wait on it and to signal it.
    let h = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE | EVENT_MODIFY_STATE, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("OpenEventW", name)) } else { Ok(Handle(h)) }
}

fn open_mutex(name: &str) -> Result<Handle, ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named mutex to wait on it and release it.
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE, 0, w.as_ptr()) };
    if h.is_null() { Err(win32("OpenMutexW", name)) } else { Ok(Handle(h)) }
}

fn open_mapping(name: &str, size: usize) -> Result<(Handle, View), ObjectsError> {
    let w = wide(name);
    // SAFETY: opens an existing named mapping for read/write.
    let h = unsafe { OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("OpenFileMappingW", name));
    }
    let h = Handle(h);
    let view = View::map(&h, name)?;
    if view.len < size {
        return Err(ObjectsError::Win32 { what: format!("{name} maps {} bytes, under {size}", view.len), code: 0 });
    }
    Ok((h, view))
}

impl Objects {
    /// The helper's side (design flow step 2): create, in order, the doorbell events, the mask
    /// region, the state lock and the state block. A new block gets the v1 header; a block that
    /// already exists (the game held it across a helper restart) is checked and, if it is not v1,
    /// refused with `VersionMismatch` without being written.
    pub fn create(pid: u32) -> Result<Objects, ObjectsError> {
        let bell_game = create_event(&bell_game_name(pid))?;
        let bell_host = create_event(&bell_host_name(pid))?;
        let (bells_mapping, bells, _) = create_mapping(&bells_name(pid), MASK_REGION_SIZE)?;
        let lock = create_mutex(&state_lock_name(pid))?;
        let (state_mapping, state, existed) = create_mapping(&state_name(pid), BLOCK_SIZE)?;
        let o = Objects {
            bell_game,
            bell_host,
            _bells_mapping: bells_mapping,
            bells,
            lock,
            _state_mapping: state_mapping,
            state,
            held: AtomicBool::new(false),
        };
        if existed {
            let g = o.lock_raw(LOCK_TIMEOUT_MS).ok_or(ObjectsError::Busy)?;
            state::check_header(g.bytes()).map_err(ObjectsError::VersionMismatch)?;
        } else {
            // Nobody else can have opened it yet: the game opens the objects only after its
            // `overlay_hello`, and the overlay pipe is created after this.
            let mut g = o.lock_raw(LOCK_TIMEOUT_MS).ok_or(ObjectsError::Busy)?;
            state::init_header(g.bytes_mut());
        }
        Ok(o)
    }

    /// The game's side (design flow step 3): open the objects the helper created, and check that
    /// the state block is layout v1.
    pub fn open(pid: u32) -> Result<Objects, ObjectsError> {
        let bell_game = open_event(&bell_game_name(pid))?;
        let bell_host = open_event(&bell_host_name(pid))?;
        let (bells_mapping, bells) = open_mapping(&bells_name(pid), MASK_REGION_SIZE)?;
        let lock = open_mutex(&state_lock_name(pid))?;
        let (state_mapping, state) = open_mapping(&state_name(pid), BLOCK_SIZE)?;
        let o = Objects {
            bell_game,
            bell_host,
            _bells_mapping: bells_mapping,
            bells,
            lock,
            _state_mapping: state_mapping,
            state,
            held: AtomicBool::new(false),
        };
        {
            let g = o.lock(LOCK_TIMEOUT_MS)?;
            state::check_header(g.bytes()).map_err(ObjectsError::VersionMismatch)?;
        }
        Ok(o)
    }

    fn mask(&self, offset: usize) -> &AtomicU64 {
        debug_assert!(offset + 8 <= MASK_REGION_SIZE && self.bells.len >= MASK_REGION_SIZE);
        // SAFETY: the view is page-aligned and at least MASK_REGION_SIZE bytes, so offsets 0 and 8
        // are in bounds and 8-byte aligned; it stays mapped while `self` lives; and every access
        // to these words, in every process, is atomic.
        unsafe { AtomicU64::from_ptr(self.bells.ptr.add(offset).cast::<u64>()) }
    }

    /// Host -> game: OR `bits` into `to_game`, then signal `bell-game`. Never waits.
    pub fn ring_game(&self, bits: u64) {
        self.mask(TO_GAME_OFFSET).fetch_or(bits, Ordering::AcqRel);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(self.bell_game.0) };
    }

    /// Game -> host: OR `bits` into `to_host`, then signal `bell-host`. Never waits, so any game
    /// thread may ring.
    pub fn ring_host(&self, bits: u64) {
        self.mask(TO_HOST_OFFSET).fetch_or(bits, Ordering::AcqRel);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(self.bell_host.0) };
    }

    /// The game's receive: swap `to_game` to 0 and return what was there.
    pub fn take_to_game(&self) -> u64 {
        self.mask(TO_GAME_OFFSET).swap(0, Ordering::AcqRel)
    }

    /// The helper's receive: swap `to_host` to 0 and return what was there.
    pub fn take_to_host(&self) -> u64 {
        self.mask(TO_HOST_OFFSET).swap(0, Ordering::AcqRel)
    }

    /// The `bell-game` event, for the game's worker to wait on.
    pub fn bell_game(&self) -> HANDLE {
        self.bell_game.0
    }

    /// The `bell-host` event, for the helper's loop to wait on.
    pub fn bell_host(&self) -> HANDLE {
        self.bell_host.0
    }

    /// Take the state lock within `timeout_ms` (0 for game threads other than the worker). A lock
    /// whose owner died holding it counts as acquired, after the header is re-checked (design
    /// "Failures"). One guard at a time: while this thread holds one, `lock` returns `Busy`.
    pub fn lock(&self, timeout_ms: u32) -> Result<StateGuard<'_>, LockError> {
        // SAFETY: waits on our mutex handle.
        let r = unsafe { WaitForSingleObject(self.lock.0, timeout_ms) };
        match r {
            WAIT_OBJECT_0 => self.guard(),
            WAIT_ABANDONED => {
                let g = self.guard()?;
                match state::check_header(g.bytes()) {
                    Ok(()) => Ok(g),
                    Err(h) => Err(LockError::BadHeader(h)), // `g` drops here: released
                }
            }
            _ => Err(LockError::Busy),
        }
    }

    /// `lock` without the header check, for `create` (a new block has no header yet).
    fn lock_raw(&self, timeout_ms: u32) -> Option<StateGuard<'_>> {
        // SAFETY: waits on our mutex handle.
        let r = unsafe { WaitForSingleObject(self.lock.0, timeout_ms) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED { self.guard().ok() } else { None }
    }

    /// The mutex was just acquired: hand out the one guard, or undo a recursive acquire.
    fn guard(&self) -> Result<StateGuard<'_>, LockError> {
        if self.held.swap(true, Ordering::Acquire) {
            // This thread already holds a guard (only the owner can acquire a held mutex).
            // SAFETY: releases the extra, recursive acquisition made just now.
            unsafe { ReleaseMutex(self.lock.0) };
            return Err(LockError::Busy);
        }
        Ok(StateGuard { o: self, _not_send: PhantomData })
    }
}

/// The state block while its lock is held. Released on drop. Not `Send`: a mutex is owned by the
/// thread that took it, so the guard must be dropped there.
pub struct StateGuard<'a> {
    o: &'a Objects,
    _not_send: PhantomData<*const ()>,
}

impl StateGuard<'_> {
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the view holds at least BLOCK_SIZE bytes (checked when mapped) and the lock is
        // held, so no other thread or process writes it now.
        unsafe { std::slice::from_raw_parts(self.o.state.ptr, BLOCK_SIZE) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`; `&mut self` makes this the only borrow.
        unsafe { std::slice::from_raw_parts_mut(self.o.state.ptr, BLOCK_SIZE) }
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        // Clear the flag before the release, so the next owner finds it clear.
        self.o.held.store(false, Ordering::Release);
        // SAFETY: releases the mutex this thread acquired for this guard.
        unsafe { ReleaseMutex(self.o.lock.0) };
    }
}
```

- [ ] **Step 6: Write the two probes and the gate test**

`crates/uw-game-channel/examples/game_probe.rs` (complete file):

```rust
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
```

`crates/uw-game-channel/examples/helper_probe.rs` (complete file):

```rust
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
```

`crates/uw-game-channel/tests/wine_objects.rs` (complete file):

```rust
#![cfg(unix)]
//! The plan 2a gate under plain Wine (system `/usr/bin/wine`, not Proton): two Wine processes in
//! one scratch prefix share the channel objects. `game_probe.exe` opens them as the in-game worker
//! will, `helper_probe.exe` creates them as the helper will; between them they prove the named
//! events, the mask region's lock-free atomics (with a lost-update check), the state block, the
//! one-guard rule, and an abandoned lock (taken as acquired, header re-checked) across processes.
//! See the two examples for what each checks.
//!
//! Ignored by default (needs Wine and the Windows artifacts). Build and run:
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
//!   timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
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
    assert!(p.exists(), "missing {} (build the examples first)", p.display());
    p
}

/// Wait up to `d` for `child` to exit, then kill it; the exit status if it exited by itself.
fn wait_bounded(child: &mut Child, d: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < d {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// The scratch prefix and both probes. Dropping it (on success or panic) kills the probes, ends
/// this prefix's wineserver only, and removes the prefix.
struct Fixture {
    dir: PathBuf,
    game: Option<Child>,
    helper: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for c in [self.helper.take(), self.game.take()].into_iter().flatten() {
            let mut c = c;
            let _ = c.kill();
            wait_bounded(&mut c, Duration::from_secs(10));
        }
        if let Ok(mut k) = Command::new(wineserver()).arg("-k").env("WINEPREFIX", &self.dir).spawn() {
            wait_bounded(&mut k, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(self.dir.parent().unwrap_or(&self.dir));
    }
}

fn wine_cmd(dir: &Path, program: &Path) -> Command {
    let mut c = Command::new(wine());
    c.arg(program)
        .env("WINEPREFIX", dir)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "mscoree,mshtml=")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn drain(child: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut s);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut s);
    }
    s
}

/// Catches the design's open risk: events, the mask atomics or the state block not shared
/// between two Wine processes, a doorbell lost, or a bit lost to a non-atomic OR or swap. Also
/// catches a second guard on one thread, and an abandoned lock that is not taken as acquired or
/// whose header is not re-checked (design "Failures").
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu examples"]
fn the_channel_objects_work_between_two_wine_processes() {
    let dir = std::env::temp_dir().join(format!("uw-inject-gate-{}", std::process::id())).join("pfx");
    std::fs::create_dir_all(&dir).unwrap();
    let mut fx = Fixture { dir, game: None, helper: None };

    fx.game = Some(wine_cmd(&fx.dir, &example("game_probe.exe")).spawn().unwrap());
    // Its first line is `pid <n>`. The first run boots the prefix, so allow up to 200 s.
    let stdout = fx.game.as_mut().unwrap().stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next().and_then(Result::ok).unwrap_or_default();
        let _ = tx.send(first);
        // Keep the rest (the final `ok`) for the assertion below.
        let rest: Vec<String> = lines.map_while(Result::ok).collect();
        let _ = tx.send(rest.join("\n"));
    });
    let first = rx.recv_timeout(Duration::from_secs(200)).expect("game_probe printed no pid");
    let pid = first.strip_prefix("pid ").unwrap_or_else(|| panic!("unexpected first line {first:?}")).to_string();

    fx.helper = Some(wine_cmd(&fx.dir, &example("helper_probe.exe")).arg(&pid).spawn().unwrap());
    let helper_status = wait_bounded(fx.helper.as_mut().unwrap(), Duration::from_secs(120));
    let helper_out = drain(fx.helper.as_mut().unwrap());
    let game_status = wait_bounded(fx.game.as_mut().unwrap(), Duration::from_secs(30));
    let game_rest = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let game_err = drain(fx.game.as_mut().unwrap());

    assert!(
        helper_status.is_some_and(|s| s.success()) && helper_out.contains("ok rounds=200 sends=[2000, 2000]"),
        "helper_probe: {helper_status:?}\n{helper_out}\ngame_probe: {game_status:?}\n{game_rest}\n{game_err}"
    );
    assert!(
        game_status.is_some_and(|s| s.success()) && game_rest.contains("ok"),
        "game_probe: {game_status:?}\n{game_rest}\n{game_err}\nhelper_probe:\n{helper_out}"
    );
}
```

- [ ] **Step 7: Build for Windows and run both clippies**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
cargo clippy --target x86_64-pc-windows-gnu -p uw-game-channel -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy -p uw-game-channel -p uw-pipe-proto --all-targets -- -D warnings
```
Expected: `Finished` each time, no warnings; `target/x86_64-pc-windows-gnu/release/examples/` holds `game_probe.exe` and `helper_probe.exe`.

- [ ] **Step 8: Run the gate (GATE)**

Run:
```bash
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: `the_channel_objects_work_between_two_wine_processes ... ok`, `test result: ok. 1 passed` (about 7 s, most of it booting the prefix); the leftover check prints nothing.

**If it fails, STOP.** Do not go on to Task 2: report to the owner that named events, the mask atomics, the state block or its lock do not work between two Wine processes as the design needs, with the test's output (it prints both probes' stdout and stderr). The design's doorbell scheme depends on this.

- [ ] **Step 9: Commit**

```bash
git add Cargo.lock crates/uw-pipe-proto crates/uw-game-channel
git commit -m "channel: prove the shared channel objects between two Wine processes"
```

- [ ] **Step 10: Check that the gate bites, then restore**

Make the `to_host` mask non-atomic in `crates/uw-game-channel/src/objects.rs` (a lost update becomes possible), in two places. Replace `        self.mask(TO_HOST_OFFSET).fetch_or(bits, Ordering::AcqRel);` with

```rust
        let m = self.mask(TO_HOST_OFFSET);
        let v = m.load(Ordering::Acquire);
        std::thread::yield_now();
        m.store(v | bits, Ordering::Release);
```

and `        self.mask(TO_HOST_OFFSET).swap(0, Ordering::AcqRel)` with

```rust
        {
            let m = self.mask(TO_HOST_OFFSET);
            let v = m.load(Ordering::Acquire);
            std::thread::yield_now();
            m.store(0, Ordering::Release);
            v
        }
```

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
git checkout -- crates/uw-game-channel/src/objects.rs
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: the first test run FAILS with `lost-update check stalled at […] of 2000` from `helper_probe` and `send … was never acknowledged (lost)` from `game_probe`; after the restore it passes again (`1 passed`); `git status` is clean; no leftovers.

---

## Task 2: `uw-pipe-proto`: overlay pipe messages, the overlay pipe name, bit names

Pure additions, unit-tested on Linux (design "Overlay pipe messages", "Doorbell bits v1"). `pipe.rs` gets a small refactor so both pipes share the 16-byte `UWP1` header code (`frame`, `unframe`); plan 1's 14 pipe tests guard it.

**Files:**
- Modify: `crates/uw-pipe-proto/src/pipe.rs`, `crates/uw-pipe-proto/src/names.rs`, `crates/uw-pipe-proto/src/bells.rs`, `crates/uw-pipe-proto/tests/pipe.rs`
- Create: `crates/uw-pipe-proto/tests/bells.rs`

- [ ] **Step 1: Write the failing tests**

`crates/uw-pipe-proto/tests/bells.rs` (complete file):

```rust
use uw_pipe_proto::bells::{bit_names, to_game, to_host, MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET};

/// Catches a mask region that disagrees with the design (to_game u64 at 0, to_host u64 at 8).
#[test]
fn the_mask_region_matches_the_design() {
    assert_eq!((MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET), (16, 0, 8));
}

/// Catches a renumbered bit: the overlay and the helper must agree on v1's bit numbers.
#[test]
fn the_v1_bits_have_the_design_numbers() {
    assert_eq!(
        [to_game::EDIT_MODE_ON, to_game::EDIT_MODE_OFF, to_game::SHOW, to_game::HIDE],
        [1 << 0, 1 << 1, 1 << 2, 1 << 3]
    );
    assert_eq!(to_host::STATE_APPLIED, 1 << 0);
}

/// Catches unknown bits dropped from the `bell` event, or named some other way than `bit<N>`.
#[test]
fn bits_are_named_and_unknown_ones_are_bit_n() {
    assert_eq!(bit_names(to_host::STATE_APPLIED, &to_host::NAMES), ["state_applied"]);
    assert_eq!(bit_names(to_host::STATE_APPLIED | 1 << 5 | 1 << 63, &to_host::NAMES), ["state_applied", "bit5", "bit63"]);
    assert_eq!(
        bit_names(0b1111, &to_game::NAMES),
        ["edit_mode_on", "edit_mode_off", "show", "hide"]
    );
    assert!(bit_names(0, &to_host::NAMES).is_empty());
}
```

In `crates/uw-pipe-proto/tests/pipe.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
    v
}
```

with:

```rust
    v
}

// --- The overlay pipe (plan 2a) ---

use uw_pipe_proto::pipe::{check_overlay_first, decode_overlay, encode_overlay, OverlayFirst, OverlayMessage};

#[test]
fn overlay_messages_round_trip() {
    for m in [
        OverlayMessage::Hello { module_version: 3, state_layout: 1 },
        OverlayMessage::Ping { nonce: 77 },
        OverlayMessage::Pong { nonce: 77, seen_seq: u64::MAX - 1 },
    ] {
        assert_eq!(decode_overlay(&encode_overlay(&m)).unwrap(), m);
    }
}

/// Catches the payload layout drifting from the design table (hello 8 bytes, ping 4, pong 16).
#[test]
fn overlay_payloads_have_the_design_sizes_and_types() {
    let hello = encode_overlay(&OverlayMessage::Hello { module_version: 3, state_layout: 1 });
    assert_eq!((u16::from_le_bytes([hello[4], hello[5]]), hello.len() - 16), (0x0101, 8));
    let ping = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    assert_eq!((u16::from_le_bytes([ping[4], ping[5]]), ping.len() - 16), (0x0102, 4));
    let pong = encode_overlay(&OverlayMessage::Pong { nonce: 1, seen_seq: 9 });
    assert_eq!((u16::from_le_bytes([pong[4], pong[5]]), pong.len() - 16), (0x0103, 16));
    assert_eq!(u64::from_le_bytes(pong[24..32].try_into().unwrap()), 9, "seen_seq at payload offset 8");
}

/// Catches one pipe accepting the other pipe's messages (design: types from 0x0100 are rejected
/// on the bootstrap pipe, and the bootstrap's on the overlay pipe).
#[test]
fn a_message_from_the_other_pipe_is_an_unknown_type() {
    let status = encode(&Message::Status { request_id: 1, code: StatusCode::Ok, detail: 0, message: String::new() });
    assert!(matches!(decode_overlay(&status), Err(ProtoError::UnknownType(3))));
    let ping = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    assert!(matches!(decode(&ping), Err(ProtoError::UnknownType(0x0102))));
}

#[test]
fn short_bad_magic_and_truncated_overlay_messages_are_rejected() {
    assert!(matches!(decode_overlay(&[0u8; 15]), Err(ProtoError::Short)));
    let mut bytes = encode_overlay(&OverlayMessage::Ping { nonce: 1 });
    bytes[0] = b'X';
    assert!(matches!(decode_overlay(&bytes), Err(ProtoError::BadMagic)));
    // A pong whose payload stops after the nonce.
    assert!(matches!(decode_overlay(&frame(0x0103, &1u32.to_le_bytes())), Err(ProtoError::Truncated)));
}

/// Catches the helper accepting a first message that is not a supported `overlay_hello`.
#[test]
fn the_first_overlay_message_must_be_a_supported_hello() {
    let hello = |layout| encode_overlay(&OverlayMessage::Hello { module_version: 5, state_layout: layout });
    assert_eq!(check_overlay_first(&hello(1)), OverlayFirst::Hello { module_version: 5 });
    // Another state layout, or another header version: version_mismatch, not pipe_error.
    assert_eq!(check_overlay_first(&hello(2)), OverlayFirst::VersionMismatch);
    let mut newer = hello(1);
    newer[6..8].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(check_overlay_first(&newer), OverlayFirst::VersionMismatch);
    // A ping, a bootstrap hello, or garbage first: pipe_error.
    assert!(matches!(check_overlay_first(&encode_overlay(&OverlayMessage::Ping { nonce: 1 })), OverlayFirst::Bad(_)));
    let bootstrap_hello = encode(&Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        bootstrap_version: 1,
        bitness: 64,
        overlay_loaded: false,
        overlay_path: String::new(),
    });
    assert!(matches!(check_overlay_first(&bootstrap_hello), OverlayFirst::Bad(_)));
    assert!(matches!(check_overlay_first(b"not a frame at all"), OverlayFirst::Bad(_)));
}
```


- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test bells --test pipe`
Expected: does not compile: unresolved imports `bit_names`, `to_game::NAMES`/`to_host::NAMES` (no `NAMES`), and `check_overlay_first`, `decode_overlay`, `encode_overlay`, `OverlayFirst`, `OverlayMessage`.

- [ ] **Step 3: Add the overlay messages, the overlay pipe name and the bit names**

In `crates/uw-pipe-proto/src/pipe.rs`, make these 7 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! at most `MAX_PAYLOAD` bytes. Paths are UTF-16 with a `u16` code-unit-count prefix (<= 1024);
//! status messages are UTF-8 with a `u16` byte-count prefix (<= 256).

pub const MAGIC: [u8; 4] = *b"UWP1";
```

with:

```rust
//! at most `MAX_PAYLOAD` bytes. Paths are UTF-16 with a `u16` code-unit-count prefix (<= 1024);
//! status messages are UTF-8 with a `u16` byte-count prefix (<= 256).
//!
//! Two pipes use this framing. The bootstrap pipe carries `Message` (types 1-3); the overlay pipe
//! carries `OverlayMessage` (types from 0x0100), so a message sent to the wrong pipe is rejected
//! as `UnknownType`.

pub const MAGIC: [u8; 4] = *b"UWP1";
```

2. Replace:

```rust
const TYPE_LOAD_OVERLAY: u16 = 2;
const TYPE_STATUS: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
```

with:

```rust
const TYPE_LOAD_OVERLAY: u16 = 2;
const TYPE_STATUS: u16 = 3;
const TYPE_OVERLAY_HELLO: u16 = 0x0101;
const TYPE_PING: u16 = 0x0102;
const TYPE_PONG: u16 = 0x0103;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
```

3. Replace:

```rust
        }
    };
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    // `pid` is reserved (the sender's Windows pid); 0 here, filled and checked by plan 2.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}
```

with:

```rust
        }
    };
    frame(ty, &payload)
}

/// The 16-byte header, then the payload.
fn frame(ty: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    // `pid` is reserved (the sender's Windows pid); always 0, not checked.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}
```

4. Replace:

```rust
        self.i = end;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
```

with:

```rust
        self.i = end;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, ProtoError> {
        let end = self.i + 8;
        let s = self.b.get(self.i..end).ok_or(ProtoError::Truncated)?;
        self.i = end;
        Ok(u64::from_le_bytes(s.try_into().unwrap()))
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
```

5. Replace:

```rust
}

/// Decode one framed message. A string-length field over its cap fails before any allocation.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtoError::Short);
```

with:

```rust
}

/// Check the header; the message type and the payload it frames.
fn unframe(bytes: &[u8]) -> Result<(u16, &[u8]), ProtoError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtoError::Short);
```

6. Replace:

```rust
    }
    let payload = bytes.get(HEADER_LEN..HEADER_LEN + payload_len).ok_or(ProtoError::Truncated)?;
    let mut c = Cur { b: payload, i: 0 };
    match ty {
```

with:

```rust
    }
    let payload = bytes.get(HEADER_LEN..HEADER_LEN + payload_len).ok_or(ProtoError::Truncated)?;
    Ok((ty, payload))
}

/// Decode one framed bootstrap-pipe message. A string-length field over its cap fails before any
/// allocation. An overlay-pipe type is `UnknownType`.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    let (ty, payload) = unframe(bytes)?;
    let mut c = Cur { b: payload, i: 0 };
    match ty {
```

7. Replace:

```rust
        other => Err(ProtoError::UnknownType(other)),
    }
}
```

with:

```rust
        other => Err(ProtoError::UnknownType(other)),
    }
}

/// The overlay pipe (`\\.\pipe\underwolf-<pid>-overlay`): the in-game overlay's own connection to
/// the helper (design "Overlay pipe messages").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayMessage {
    /// game -> helper, always first: the overlay module's version and the state block layout it
    /// reads.
    Hello { module_version: u32, state_layout: u16 },
    /// helper -> game.
    Ping { nonce: u32 },
    /// game -> helper: the ping's nonce and the last `seq` the overlay applied.
    Pong { nonce: u32, seen_seq: u64 },
}

/// Encode an overlay-pipe message (header + payload). Reserved fields are 0.
pub fn encode_overlay(m: &OverlayMessage) -> Vec<u8> {
    let mut p = Vec::new();
    let ty = match m {
        OverlayMessage::Hello { module_version, state_layout } => {
            p.extend_from_slice(&module_version.to_le_bytes());
            p.extend_from_slice(&state_layout.to_le_bytes());
            p.extend_from_slice(&0u16.to_le_bytes());
            TYPE_OVERLAY_HELLO
        }
        OverlayMessage::Ping { nonce } => {
            p.extend_from_slice(&nonce.to_le_bytes());
            TYPE_PING
        }
        OverlayMessage::Pong { nonce, seen_seq } => {
            p.extend_from_slice(&nonce.to_le_bytes());
            p.extend_from_slice(&0u32.to_le_bytes());
            p.extend_from_slice(&seen_seq.to_le_bytes());
            TYPE_PONG
        }
    };
    frame(ty, &p)
}

/// Decode one framed overlay-pipe message. A bootstrap-pipe type is `UnknownType`.
pub fn decode_overlay(bytes: &[u8]) -> Result<OverlayMessage, ProtoError> {
    let (ty, payload) = unframe(bytes)?;
    let mut c = Cur { b: payload, i: 0 };
    match ty {
        TYPE_OVERLAY_HELLO => {
            let module_version = c.u32()?;
            let state_layout = c.u16()?;
            c.u16()?; // reserved
            Ok(OverlayMessage::Hello { module_version, state_layout })
        }
        TYPE_PING => Ok(OverlayMessage::Ping { nonce: c.u32()? }),
        TYPE_PONG => {
            let nonce = c.u32()?;
            c.u32()?; // reserved
            Ok(OverlayMessage::Pong { nonce, seen_seq: c.u64()? })
        }
        other => Err(ProtoError::UnknownType(other)),
    }
}

/// How the helper takes the first message on a new overlay-pipe connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayFirst {
    /// An `overlay_hello` the helper supports.
    Hello { module_version: u32 },
    /// An `overlay_hello` with another header version or state layout (design: `version_mismatch`).
    VersionMismatch,
    /// Anything else (design: `pipe_error`; the connection is dropped).
    Bad(String),
}

/// Classify the first overlay-pipe message (design: it must be `overlay_hello`; a header version
/// or `state_layout` the helper does not support gives `version_mismatch`).
pub fn check_overlay_first(bytes: &[u8]) -> OverlayFirst {
    match decode_overlay(bytes) {
        Ok(OverlayMessage::Hello { module_version, state_layout }) if state_layout == crate::state::LAYOUT_VERSION => {
            OverlayFirst::Hello { module_version }
        }
        Ok(OverlayMessage::Hello { .. }) | Err(ProtoError::BadVersion(_)) => OverlayFirst::VersionMismatch,
        Ok(_) => OverlayFirst::Bad("expected overlay_hello first".into()),
        Err(e) => OverlayFirst::Bad(format!("decode overlay_hello: {e}")),
    }
}
```


In `crates/uw-pipe-proto/src/names.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
pub fn helper_mutex_name(pid: u32) -> String {
```

with:

```rust
pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}")
}
/// The overlay's own pipe (design 2a: the overlay connects separately from the bootstrap).
pub fn overlay_pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\underwolf-{pid}-overlay")
}
pub fn helper_mutex_name(pid: u32) -> String {
```

2. Replace:

```rust
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
```

with:

```rust
    fn names_embed_the_pid() {
        assert_eq!(pipe_name(1234), r"\\.\pipe\underwolf-1234");
        assert_eq!(overlay_pipe_name(1234), r"\\.\pipe\underwolf-1234-overlay");
        assert_eq!(helper_mutex_name(1234), r"Local\underwolf-helper-1234");
        assert_eq!(marker_mutex_name(1234), r"Local\underwolf-bootstrap-1234");
```


In `crates/uw-pipe-proto/src/bells.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
    pub const SHOW: u64 = 1 << 2;
    pub const HIDE: u64 = 1 << 3;
}
```

with:

```rust
    pub const SHOW: u64 = 1 << 2;
    pub const HIDE: u64 = 1 << 3;
    /// The wire names, for `bit_names`.
    pub const NAMES: [(u64, &str); 4] =
        [(EDIT_MODE_ON, "edit_mode_on"), (EDIT_MODE_OFF, "edit_mode_off"), (SHOW, "show"), (HIDE, "hide")];
}
```

2. Replace:

```rust
pub mod to_host {
    pub const STATE_APPLIED: u64 = 1 << 0;
}
```

with:

```rust
pub mod to_host {
    pub const STATE_APPLIED: u64 = 1 << 0;
    /// The wire names, for `bit_names`.
    pub const NAMES: [(u64, &str); 1] = [(STATE_APPLIED, "state_applied")];
}

/// The names of the bits set in `bits`, lowest bit first. A bit missing from `known` is named
/// `bit<N>` (design: the JSON `bell` event names unknown bits that way).
pub fn bit_names(bits: u64, known: &[(u64, &str)]) -> Vec<String> {
    (0..64)
        .map(|n| 1u64 << n)
        .filter(|bit| bits & bit != 0)
        .map(|bit| match known.iter().find(|(b, _)| *b == bit) {
            Some((_, name)) => name.to_string(),
            None => format!("bit{}", bit.trailing_zeros()),
        })
        .collect()
}
```


- [ ] **Step 4: Run the tests and both clippies**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto --all-targets -- -D warnings
```
Expected: names 2, `bells` 3, `jsonl` 5, `pipe` 19, `state` 5, all pass; both clippies clean.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "channel: overlay pipe messages, the overlay pipe name and bit names"
```

---

## Task 3: JSON lines with request ids, the new commands and events

Design "Linux ↔ helper (JSON lines)" and "Plan 1 items": every command carries a `request_id` that its one reply echoes; `set_state` and `ping`; `state_set`, `pong`, `overlay_connected`, `overlay_disconnected`, `bell`, `bootstrap_disconnected`, `game_exited` (now an event of its own, not an `error`); `bootstrap_connected` carries `overlay_path` (plan 1 final review M3); `uw-host`'s four events; `Command::parse_with_id` (the CLI's stdin: the command always gets an id of the host's, the user's own id comes back to be put on the reply) and `Event::request_id`/`request_id_mut`. The change ripples into plan 1's users, which are updated minimally here so that plan 1's behaviour and both Wine suites still hold:
- `decide`: the new `bootstrap_connected` and `game_exited` mapping.
- The helper relay: the command's `request_id` is the pipe request id (the bootstrap already echoes it); a failed write is reported as `bootstrap_disconnected` with a `bootstrap_not_connected` reply, not `game_exited` (design "Plan 1 items"); `set_state` and `ping` fail with `overlay_not_connected` (the overlay pipe comes in Task 6-7).
- `uw-inject`'s `Driver`: numbers its `load_overlay`.

**Files:**
- Modify: `crates/uw-pipe-proto/src/jsonl.rs`, `crates/uw-pipe-proto/src/lib.rs`, `crates/uw-pipe-proto/tests/jsonl.rs`
- Modify: `crates/uw-helper/src/decide.rs`, `crates/uw-helper/tests/decide.rs`, `crates/uw-helper/src/bin/uw-helper64.rs`, `crates/uw-helper/tests/wine_e2e.rs`
- Modify: `crates/uw-inject/src/driver.rs`, `crates/uw-inject/tests/discovery.rs`

- [ ] **Step 1: Write the failing JSON-lines tests**

`crates/uw-pipe-proto/tests/jsonl.rs` (complete file):

```rust
use uw_pipe_proto::jsonl::{Command, Event};

#[test]
fn load_overlay_command_round_trips() {
    let c = Command::LoadOverlay { request_id: 1, path: r"Z:\games\overlay.dll".into() };
    let line = c.to_line();
    assert_eq!(line, r#"{"cmd":"load_overlay","request_id":1,"path":"Z:\\games\\overlay.dll"}"#);
    assert_eq!(Command::parse(&line).unwrap(), c);
}

/// Catches a renamed command or field: these are the wire names in the design's table.
#[test]
fn set_state_and_ping_have_the_design_wire_form() {
    let c = Command::SetState { request_id: 2, edit_mode: Some(true), visible: None };
    assert_eq!(c.to_line(), r#"{"cmd":"set_state","request_id":2,"edit_mode":true}"#);
    assert_eq!(Command::parse(&c.to_line()).unwrap(), c);
    assert_eq!(
        Command::parse(r#"{"cmd":"set_state","request_id":3,"visible":false}"#).unwrap(),
        Command::SetState { request_id: 3, edit_mode: None, visible: Some(false) }
    );
    assert_eq!(Command::Ping { request_id: 4 }.to_line(), r#"{"cmd":"ping","request_id":4}"#);
    assert_eq!(Command::Ping { request_id: 4 }.request_id(), 4);
}

/// Catches the helper accepting a command without an id (its reply could not be matched).
#[test]
fn a_command_without_a_request_id_is_rejected_by_parse() {
    assert!(Command::parse(r#"{"cmd":"ping"}"#).is_err());
}

/// Catches `uw-inject` sending a user's id on to the helper (it could collide with an id the host
/// assigned), losing it (the reply could not carry it back), or accepting one that is not a u32.
#[test]
fn parse_with_id_gives_the_host_id_and_returns_the_users() {
    assert_eq!(Command::parse_with_id(r#"{"cmd":"ping"}"#, 41).unwrap(), (Command::Ping { request_id: 41 }, None));
    assert_eq!(
        Command::parse_with_id(r#"{"cmd":"ping","request_id":7}"#, 41).unwrap(),
        (Command::Ping { request_id: 41 }, Some(7))
    );
    assert!(Command::parse_with_id(r#"{"cmd":"ping","request_id":-1}"#, 41).is_err());
    assert!(Command::parse_with_id("not json", 41).is_err());
}

/// Catches a reply whose id cannot be read or replaced, or an unsolicited event taken for a reply.
#[test]
fn replies_expose_their_request_id() {
    let mut e = Event::Pong { request_id: 3, seen_seq: 0 };
    *e.request_id_mut().unwrap() = 9;
    assert_eq!(e, Event::Pong { request_id: 9, seen_seq: 0 });
    let mut e = Event::reply_error(4, "state_busy");
    *e.request_id_mut().unwrap() = 8;
    assert_eq!(e, Event::reply_error(8, "state_busy"));
    assert_eq!(Event::StateSet { request_id: 5, seq: 1 }.request_id(), Some(5));
    let status = Event::Status { request_id: 6, code: "ok".into(), detail: 0, message: String::new() };
    assert_eq!(status.request_id(), Some(6));
    assert_eq!(Event::error("no_window").request_id(), None);
    assert!(Event::error("no_window").request_id_mut().is_none());
    assert_eq!(Event::GameExited.request_id(), None);
}

#[test]
fn events_round_trip() {
    for e in [
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None },
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\g\overlay.dll".into()) },
        Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: "overlay started".into() },
        Event::Status { request_id: 2, code: "load_failed".into(), detail: 126, message: "LoadLibraryW failed".into() },
        Event::StateSet { request_id: 3, seq: 9 },
        Event::Pong { request_id: 4, seen_seq: 9 },
        Event::OverlayConnected { module_version: 1 },
        Event::OverlayDisconnected,
        Event::Bell { bits: vec!["state_applied".into(), "bit5".into()], applied_seq: 9 },
        Event::BootstrapDisconnected,
        Event::GameExited,
        Event::error("no_window"),
        Event::reply_error(5, "overlay_not_connected"),
        Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(1455) },
        Event::HelperRestarted { attempt: 2 },
        Event::HelperProtocolError { line: "garbage".into() },
        Event::OverlayTimeout,
        Event::Stopped { reason: "game_exited".into() },
    ] {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
    }
}

/// Catches renamed events or fields: these are the wire names in the design.
#[test]
fn events_have_the_design_wire_form() {
    assert_eq!(Event::error("no_window").to_line(), r#"{"event":"error","code":"no_window"}"#);
    assert_eq!(
        Event::reply_error(5, "overlay_not_connected").to_line(),
        r#"{"event":"error","request_id":5,"code":"overlay_not_connected"}"#
    );
    assert_eq!(
        Event::Status { request_id: 3, code: "start_failed".into(), detail: 7, message: "nonzero".into() }.to_line(),
        r#"{"event":"status","request_id":3,"code":"start_failed","detail":7,"message":"nonzero"}"#
    );
    assert_eq!(
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\o.dll".into()) }.to_line(),
        r#"{"event":"bootstrap_connected","overlay_loaded":true,"overlay_path":"Z:\\o.dll"}"#
    );
    assert_eq!(
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }.to_line(),
        r#"{"event":"bootstrap_connected","overlay_loaded":false}"#
    );
    assert_eq!(
        Event::Bell { bits: vec!["state_applied".into()], applied_seq: 2 }.to_line(),
        r#"{"event":"bell","bits":["state_applied"],"applied_seq":2}"#
    );
    assert_eq!(Event::GameExited.to_line(), r#"{"event":"game_exited"}"#);
    assert_eq!(Event::StateSet { request_id: 1, seq: 2 }.to_line(), r#"{"event":"state_set","request_id":1,"seq":2}"#);
    assert_eq!(Event::Pong { request_id: 1, seen_seq: 2 }.to_line(), r#"{"event":"pong","request_id":1,"seen_seq":2}"#);
}

#[test]
fn unknown_command_is_an_error() {
    assert!(Command::parse(r#"{"cmd":"nope","request_id":1}"#).is_err());
    assert!(Command::parse("not json").is_err());
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test jsonl`
Expected: does not compile: `Command::LoadOverlay` has no field `request_id`, no variants `SetState`, `Ping`, `StateSet`, `Pong`, …, no `reply_error`, `request_id()`, `parse_with_id` or `request_id_mut`.

- [ ] **Step 3: Replace `jsonl.rs` and update the crate docs**

`crates/uw-pipe-proto/src/jsonl.rs` (complete file):

```rust
//! The Linux-host <-> helper channel: one JSON object per line (design 2a "Linux ↔ helper").
//! Commands travel on the helper's stdin, events on its stdout (diagnostics on stderr). Every
//! command carries a `request_id`, and its one reply (`status`, `state_set`, `pong`, or an `error`)
//! echoes it. `uw-host` adds its own events (the last four `Event` variants) to the stream it
//! hands its user. Serialized with serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};

/// Host -> helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Load (and start) the overlay at this Windows path. Reply: `status`.
    LoadOverlay { request_id: u32, path: String },
    /// Set the given state fields. Reply: `state_set` with the block's `seq` (not bumped, and no
    /// bell, if nothing changed).
    SetState {
        request_id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit_mode: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible: Option<bool>,
    },
    /// Reply: `pong` from the overlay.
    Ping { request_id: u32 },
}

/// Helper -> host, and `uw-host`'s own events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The bootstrap connected and sent `hello`; `overlay_path` is the overlay it already runs.
    BootstrapConnected {
        overlay_loaded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overlay_path: Option<String>,
    },
    /// The bootstrap's reply to `load_overlay`: the lowercase code, a numeric `detail` (a Windows
    /// error or the start function's code, 0 if none) and a human message.
    Status { request_id: u32, code: String, detail: u32, message: String },
    /// The reply to `set_state`.
    StateSet { request_id: u32, seq: u64 },
    /// The overlay's reply to `ping`: the last `seq` it applied.
    Pong { request_id: u32, seen_seq: u64 },
    /// The overlay connected to its pipe and sent `overlay_hello`.
    OverlayConnected { module_version: u32 },
    OverlayDisconnected,
    /// The overlay rang `bell-host`: the bits' names (unknown ones `bit<N>`), and `applied_seq`
    /// read right after.
    Bell { bits: Vec<String>, applied_seq: u64 },
    BootstrapDisconnected,
    /// The game process ended (the helper then exits 0).
    GameExited,
    /// A failure. `request_id` is set when it answers a command; `detail` carries a
    /// `GetLastError` where there is one.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u32>,
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<u32>,
    },
    /// `uw-host`: it started the helper again (`attempt` counts restarts since the last reset).
    HelperRestarted { attempt: u32 },
    /// `uw-host`: a helper stdout line it could not parse.
    HelperProtocolError { line: String },
    /// `uw-host`: `status ok` but no `overlay_connected` within 10 s.
    OverlayTimeout,
    /// `uw-host`: it stopped for good (no more restarts), and why.
    Stopped { reason: String },
}

impl Event {
    /// An unsolicited error with no numeric detail.
    pub fn error(code: &str) -> Event {
        Event::Error { request_id: None, code: code.to_string(), detail: None }
    }

    /// An error that answers the command `request_id`.
    pub fn reply_error(request_id: u32, code: &str) -> Event {
        Event::Error { request_id: Some(request_id), code: code.to_string(), detail: None }
    }

    /// The `request_id` a reply answers (`status`, `state_set`, `pong`, or an `error` that
    /// answers a command); `None` for an unsolicited event.
    pub fn request_id(&self) -> Option<u32> {
        match self {
            Event::Status { request_id, .. } | Event::StateSet { request_id, .. } | Event::Pong { request_id, .. } => {
                Some(*request_id)
            }
            Event::Error { request_id, .. } => *request_id,
            _ => None,
        }
    }

    /// As `request_id`, to put another id on the reply (the `uw-inject` CLI maps its user's ids).
    pub fn request_id_mut(&mut self) -> Option<&mut u32> {
        match self {
            Event::Status { request_id, .. } | Event::StateSet { request_id, .. } | Event::Pong { request_id, .. } => {
                Some(request_id)
            }
            Event::Error { request_id, .. } => request_id.as_mut(),
            _ => None,
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Event serializes")
    }
    pub fn parse(line: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl Command {
    pub fn request_id(&self) -> u32 {
        match self {
            Command::LoadOverlay { request_id, .. }
            | Command::SetState { request_id, .. }
            | Command::Ping { request_id } => *request_id,
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("Command serializes")
    }
    pub fn parse(line: &str) -> Result<Command, serde_json::Error> {
        serde_json::from_str(line.trim())
    }

    /// Parse a line of the `uw-inject` CLI's stdin, where `request_id` is optional (design:
    /// "request ids optional, assigned if absent"). The command always gets `id`, an id the host
    /// assigned, so a user's id can never collide with one of the host's own; the user's id, if the
    /// line had one, is returned so the reply can carry it back.
    pub fn parse_with_id(line: &str, id: u32) -> Result<(Command, Option<u32>), serde_json::Error> {
        let mut v: serde_json::Value = serde_json::from_str(line.trim())?;
        let mut user = None;
        if let Some(obj) = v.as_object_mut() {
            if let Some(given) = obj.get("request_id") {
                user = Some(serde_json::from_value::<u32>(given.clone())?);
            }
            obj.insert("request_id".into(), id.into());
        }
        Ok((serde_json::from_value(v)?, user))
    }
}
```

In `crates/uw-pipe-proto/src/lib.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
//! Underwolf injection protocols. `pipe` is the fixed-layout binary channel between the Wine-side
//! helper and the in-game bootstrap (spec 6.3); `jsonl` is the temporary one-object-per-line
//! channel between the Linux host and the helper (plan 2 replaces it with a binary relay);
//! `names` is the per-game pipe and mutex names both sides must agree on. `bells` (doorbell bits
//! and the mask region) and `state` (the state block, layout v1) are the control channel's shared
//! memory layouts.

pub mod bells;
```

with:

```rust
//! Underwolf injection and control-channel protocols. `pipe` is the fixed-layout binary framing
//! of the helper's two pipes (bootstrap and overlay, spec 6.3); `jsonl` is the one-object-per-line
//! channel between the Linux host and the helper; `names` is the per-game pipe, mutex, event and
//! mapping names every side must agree on. `bells` (doorbell bits and the mask region) and `state`
//! (the state block, layout v1) are the control channel's shared-memory layouts.

pub mod bells;
```


Run: `cargo test -p uw-pipe-proto`
Expected: names 2, `bells` 3, `jsonl` 8, `pipe` 19, `state` 5, all pass. (`cargo build -p uw-helper -p uw-inject` fails now; the next steps fix it.)

- [ ] **Step 4: Update `decide` and its tests**

In `crates/uw-helper/src/decide.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
    pub fn to_event(&self) -> Event {
        match self {
            Outcome::Connected { overlay_loaded, .. } => {
                Event::BootstrapConnected { overlay_loaded: *overlay_loaded }
            }
            // `hook_failed` keeps its GetLastError as the event's `detail` (design failure table).
            Outcome::HookFailed(e) => Event::Error { code: "hook_failed".into(), detail: Some(*e) },
            Outcome::AlreadyRunning => Event::error("already_running"),
            Outcome::NoWindow => Event::error("no_window"),
            Outcome::BootstrapTimeout => Event::error("bootstrap_timeout"),
            Outcome::VersionMismatch => Event::error("version_mismatch"),
            Outcome::GameExited => Event::error("game_exited"),
            // `pipe_error` is a helper-local I/O failure not in the design's failure table; it maps
            // here so the host still sees a structured error rather than a silent exit.
```

with:

```rust
    pub fn to_event(&self) -> Event {
        match self {
            Outcome::Connected { overlay_loaded, overlay_path } => {
                Event::BootstrapConnected { overlay_loaded: *overlay_loaded, overlay_path: overlay_path.clone() }
            }
            // `hook_failed` keeps its GetLastError as the event's `detail` (design failure table).
            Outcome::HookFailed(e) => Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(*e) },
            Outcome::AlreadyRunning => Event::error("already_running"),
            Outcome::NoWindow => Event::error("no_window"),
            Outcome::BootstrapTimeout => Event::error("bootstrap_timeout"),
            Outcome::VersionMismatch => Event::error("version_mismatch"),
            Outcome::GameExited => Event::GameExited,
            // `pipe_error` is a helper-local I/O failure not in the design's failure table; it maps
            // here so the host still sees a structured error rather than a silent exit.
```


In `crates/uw-helper/tests/decide.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
fn outcomes_map_to_json_events() {
    assert_eq!(
        Outcome::Connected { overlay_loaded: true, overlay_path: None }.to_event(),
        Event::BootstrapConnected { overlay_loaded: true }
    );
    assert_eq!(Outcome::NoWindow.to_event(), Event::error("no_window"));
    assert_eq!(Outcome::AlreadyRunning.to_event(), Event::error("already_running"));
    assert_eq!(Outcome::HookFailed(5).to_event(), Event::Error { code: "hook_failed".into(), detail: Some(5) });
    assert_eq!(Outcome::BootstrapTimeout.to_event(), Event::error("bootstrap_timeout"));
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
```

with:

```rust
fn outcomes_map_to_json_events() {
    assert_eq!(
        Outcome::Connected { overlay_loaded: false, overlay_path: None }.to_event(),
        Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }
    );
    // The pinned overlay's path reaches the host (plan 1 final review M3).
    assert_eq!(
        Outcome::Connected { overlay_loaded: true, overlay_path: Some(r"Z:\g\o.dll".into()) }.to_event(),
        Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\g\o.dll".into()) }
    );
    assert_eq!(Outcome::NoWindow.to_event(), Event::error("no_window"));
    assert_eq!(Outcome::AlreadyRunning.to_event(), Event::error("already_running"));
    assert_eq!(
        Outcome::HookFailed(5).to_event(),
        Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(5) }
    );
    assert_eq!(Outcome::BootstrapTimeout.to_event(), Event::error("bootstrap_timeout"));
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
```

2. Replace:

```rust
}

#[test]
fn game_exited_maps_to_its_json_event() {
    assert_eq!(Outcome::GameExited.to_event(), Event::error("game_exited"));
}
```

with:

```rust
}

/// Catches `game_exited` sent as an `error` (design 2a: it is an event of its own).
#[test]
fn game_exited_maps_to_its_json_event() {
    assert_eq!(Outcome::GameExited.to_event(), Event::GameExited);
}
```


Run: `cargo test -p uw-helper --test decide`
Expected: `27 passed` (the relay changes in Step 5 do not affect this: on Linux the bin's relay is not compiled, it is `cfg(windows)`).

- [ ] **Step 5: Update the helper's relay**

In `crates/uw-helper/src/bin/uw-helper64.rs`, make these 5 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`) on stdout, commands (`load_overlay`) on stdin, diagnostics on stderr.

#[cfg(not(windows))]
```

with:

```rust
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`, …) on stdout, commands (`load_overlay`, …) on stdin, each reply echoing its
//! command's `request_id`; diagnostics on stderr.

#[cfg(not(windows))]
```

2. Replace:

```rust
        hello_buf: Vec<u8>,
        accept: Accept,
        next_request: u32,
    }
```

with:

```rust
        hello_buf: Vec<u8>,
        accept: Accept,
    }
```

3. Replace:

```rust
                hello_buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
                accept: Accept::Listening,
                next_request: 1,
            }
        }
```

with:

```rust
                hello_buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
                accept: Accept::Listening,
            }
        }
```

4. Replace:

```rust
                }
                match Command::parse(&line) {
                    Ok(Command::LoadOverlay { path }) => {
                        if path.encode_utf16().count() > MAX_PATH_UNITS {
                            emit(&Event::error("path_too_long"));
                            continue;
                        }
                        let request_id = self.next_request;
                        self.next_request += 1;
                        if !self.write_message(&Message::LoadOverlay { request_id, path }) {
                            emit(&Event::error("game_exited"));
                            return;
                        }
                        match self.read_message(RELAY_TIMEOUT_MS).and_then(|b| decode(&b).ok()) {
                            Some(Message::Status { code, detail, message, .. }) => {
                                emit(&Event::Status { code: code.as_str().into(), detail, message });
                            }
                            _ => {
                                emit(&Event::error("status_timeout"));
                            }
                        }
                    }
                    Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
```

with:

```rust
                }
                match Command::parse(&line) {
                    Ok(Command::LoadOverlay { request_id, path }) => {
                        if path.encode_utf16().count() > MAX_PATH_UNITS {
                            emit(&Event::reply_error(request_id, "path_too_long"));
                            continue;
                        }
                        // The command's id travels to the bootstrap, which echoes it in `status`.
                        if !self.write_message(&Message::LoadOverlay { request_id, path }) {
                            // The bootstrap's end is gone: `bootstrap_disconnected`, not
                            // `game_exited` (design 2a, "Plan 1 items").
                            emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                            emit(&Event::BootstrapDisconnected);
                            return;
                        }
                        match self.read_message(RELAY_TIMEOUT_MS).and_then(|b| decode(&b).ok()) {
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
```

5. Replace:

```rust
                CloseHandle(h);
            }
            emit(&Event::error("game_exited"));
            std::process::exit(0);
        });
```

with:

```rust
                CloseHandle(h);
            }
            emit(&Event::GameExited);
            std::process::exit(0);
        });
```


- [ ] **Step 6: Update `uw-inject`'s `Driver` and its tests**

In `crates/uw-inject/src/driver.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
    sent_load: bool,
    connected: bool,
}
```

with:

```rust
    sent_load: bool,
    connected: bool,
    next_request: u32,
}
```

2. Replace:

```rust
    /// `overlay` is the Windows path to load, or `None` to connect only.
    pub fn new(overlay: Option<String>) -> Driver {
        Driver { overlay, sent_load: false, connected: false }
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded } => {
                self.connected = true;
                if *overlay_loaded {
```

with:

```rust
    /// `overlay` is the Windows path to load, or `None` to connect only.
    pub fn new(overlay: Option<String>) -> Driver {
        Driver { overlay, sent_load: false, connected: false, next_request: 1 }
    }

    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded, .. } => {
                self.connected = true;
                if *overlay_loaded {
```

3. Replace:

```rust
                    Some(path) if !overlay_loaded && !self.sent_load => {
                        self.sent_load = true;
                        vec![Command::LoadOverlay { path: path.clone() }]
                    }
                    _ => vec![],
                }
            }
            Event::Status { .. } | Event::Error { .. } => vec![],
        }
    }
```

with:

```rust
                    Some(path) if !overlay_loaded && !self.sent_load => {
                        self.sent_load = true;
                        let request_id = self.next_request;
                        self.next_request += 1;
                        vec![Command::LoadOverlay { request_id, path: path.clone() }]
                    }
                    _ => vec![],
                }
            }
            _ => vec![],
        }
    }
```


In `crates/uw-inject/tests/discovery.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
    // Fresh connection with no overlay: send load_overlay once.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: false }),
        vec![Command::LoadOverlay { path: r"Z:\games\overlay.dll".into() }]
    );
    // A later status does not re-send.
    assert_eq!(d.on_event(&Event::Status { code: "ok".into(), detail: 0, message: String::new() }), vec![]);
    assert!(d.is_done());
}
```

with:

```rust
    // Fresh connection with no overlay: send load_overlay once.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }),
        vec![Command::LoadOverlay { request_id: 1, path: r"Z:\games\overlay.dll".into() }]
    );
    // A later status does not re-send.
    assert_eq!(
        d.on_event(&Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() }),
        vec![]
    );
    assert!(d.is_done());
}
```

2. Replace:

```rust
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Reconnect: the pinned bootstrap already has the overlay, so send nothing.
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: true }), vec![]);
    assert!(d.is_done());
}
```

with:

```rust
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Reconnect: the pinned bootstrap already has the overlay, so send nothing.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\games\overlay.dll".into()) }),
        vec![]
    );
    assert!(d.is_done());
}
```

3. Replace:

```rust
fn driver_with_no_overlay_sends_nothing() {
    let mut d = Driver::new(None);
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: false }), vec![]);
}
```

with:

```rust
fn driver_with_no_overlay_sends_nothing() {
    let mut d = Driver::new(None);
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }), vec![]);
}
```


- [ ] **Step 7: Update plan 1's helper Wine tests for the new wire form**

In `crates/uw-helper/tests/wine_e2e.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust

fn load_overlay_line(path_wine: &str) -> String {
    format!(r#"{{"cmd":"load_overlay","path":"{}"}}"#, path_wine.replace('\\', "\\\\"))
}
```

with:

```rust

fn load_overlay_line(path_wine: &str) -> String {
    format!(r#"{{"cmd":"load_overlay","request_id":1,"path":"{}"}}"#, path_wine.replace('\\', "\\\\"))
}
```

2. Replace:

```rust
    }
    let event = helper.next_event(Duration::from_secs(20));
    assert!(event.contains(r#""event":"error""#) && event.contains(r#""code":"game_exited""#), "got: {event}");
}
```

with:

```rust
    }
    let event = helper.next_event(Duration::from_secs(20));
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}
```


- [ ] **Step 8: Run the unit tests and both clippies**

Run:
```bash
cargo test -p uw-pipe-proto -p uw-helper -p uw-inject
cargo clippy -p uw-pipe-proto -p uw-helper -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto -p uw-helper -p uw-inject -p uw-game-channel --all-targets -- -D warnings
```
Expected: proto as in Step 3; `uw-helper` `decide` 27; `uw-inject` `cli` 2, `discovery` 16; Wine tests listed as ignored (7 + 1); both clippies clean.

- [ ] **Step 9: Run plan 1's Wine suites**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: helper suite `7 passed`, `uw-inject` suite `1 passed`; no leftovers.

- [ ] **Step 10: Commit**

```bash
git add crates/uw-pipe-proto crates/uw-helper crates/uw-inject
git commit -m "channel: JSON lines with request ids, the new commands and events"
```

---

## Task 4: `uw-host`: discovery moved, desired state, the supervisor, the runtime

The new Linux library (design "Components and flow", "Failures"). Discovery and the helper launch move from `uw-inject` (with plan 1's Task 3 review N1: the two fixes get their own tests). `driver.rs` (moved) becomes the reconcile policy: a desired state (overlay path, `edit_mode`, `visible`), `load_overlay` when `bootstrap_connected` says none is loaded, a full `set_state` on every `overlay_connected`. `supervisor.rs` is a pure state machine on a millisecond clock (request ids, restarts after 1, 2, 4, 8 s then every 10 s with a reset after 60 s of uptime, the stop rules, `overlay_timeout`, `helper_protocol_error`), unit-tested against a scripted fake helper. The stop rule follows the owner's decision: `game_exited`, `already_running`, `version_mismatch`, `hook_failed`, `bootstrap_timeout` and a failed start stop the host; `no_window`, `channel_failed` and `pipe_error` (and any other exit) restart the helper. `host.rs` runs it with real processes through a `Launch` trait; its tests use `sh` scripts as fake helpers. `uw-inject` loses its library and imports from `uw-host`; its CLI is rewritten on the runtime in Task 10.

**Files:**
- Move: `crates/uw-inject/src/discovery.rs` → `crates/uw-host/src/discovery.rs`; `crates/uw-inject/src/driver.rs` → `crates/uw-host/src/driver.rs`; `crates/uw-inject/tests/discovery.rs` → `crates/uw-host/tests/discovery.rs`; `crates/uw-inject/tests/cli.rs` → `crates/uw-host/tests/cli.rs`
- Delete: `crates/uw-inject/src/lib.rs`
- Create: `crates/uw-host/Cargo.toml`, `crates/uw-host/src/lib.rs`, `crates/uw-host/src/supervisor.rs`, `crates/uw-host/src/host.rs`, `crates/uw-host/tests/driver.rs`, `crates/uw-host/tests/supervisor.rs`, `crates/uw-host/tests/host.rs`
- Modify: `crates/uw-inject/Cargo.toml`, `crates/uw-inject/src/bin/uw-inject.rs`, `crates/uw-inject/tests/wine_e2e.rs`

- [ ] **Step 1: Move the files and create the crate**

Run:
```bash
mkdir -p crates/uw-host/src crates/uw-host/tests
git mv crates/uw-inject/src/discovery.rs crates/uw-host/src/discovery.rs
git mv crates/uw-inject/src/driver.rs crates/uw-host/src/driver.rs
git mv crates/uw-inject/tests/discovery.rs crates/uw-host/tests/discovery.rs
git mv crates/uw-inject/tests/cli.rs crates/uw-host/tests/cli.rs
git rm -q crates/uw-inject/src/lib.rs
```

`crates/uw-host/Cargo.toml` (complete file):

```toml
[package]
name = "uw-host"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The Linux host library (design 2a): find a running Proton game and its Wine session
# (`discovery`, moved from uw-inject), start the Wine-side helper and restart it when it dies,
# assign request ids, and keep the overlay at a desired state (`driver`, `supervisor`). `host`
# runs it all with real processes; `uw-inject` is a thin CLI on it.
[lib]
name = "uw_host"

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

`crates/uw-host/src/lib.rs` (complete file; `to_wine_path` and `exe_matches` move here from `uw-inject`'s old `lib.rs` unchanged):

```rust
//! The Linux host (design 2a). `discovery` finds the game and its Wine session (unix only);
//! `driver` holds the desired state and the reconcile commands; `supervisor` is the pure
//! supervision state machine (request ids, restarts, stop rules, the overlay timeout); `host`
//! runs it with real helper processes (unix only).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub mod driver;
pub mod supervisor;

#[cfg(unix)]
pub mod discovery;
#[cfg(unix)]
pub mod host;

/// A Unix path as Wine's `Z:` drive sees it (used for both the bootstrap and overlay paths).
pub fn to_wine_path(path: &std::path::Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!("Z:{}", abs.to_string_lossy().replace('/', "\\"))
}

/// True when `name` (a bare exe name or a Windows/Unix path) names the exe `target`.
pub fn exe_matches(name: &str, target: &str) -> bool {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    base.eq_ignore_ascii_case(target)
}

/// Request ids for commands to the helper, shared by the host's user and the host's own reconcile
/// commands so the two never collide. Clones share one counter; the first id is 1.
#[derive(Debug, Clone, Default)]
pub struct RequestIds(Arc<AtomicU32>);

impl RequestIds {
    pub fn next(&self) -> u32 {
        self.0.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
    }
}
```

In `crates/uw-host/src/discovery.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
//! game's `WINEPREFIX` and `WINE*SYNC` settings (from `/proc/<pid>/environ`), its wineserver
//! socket, and Proton's `files` directory (from the prefix's `config_info`). Ported and renamed
//! from the injection spike; no dependency on `spikes/`.

use std::ffi::{OsStr, OsString};
```

with:

```rust
//! game's `WINEPREFIX` and `WINE*SYNC` settings (from `/proc/<pid>/environ`), its wineserver
//! socket, and Proton's `files` directory (from the prefix's `config_info`). Ported and renamed
//! from the injection spike (by plan 1, into uw-inject; moved here by plan 2a); no dependency on
//! `spikes/`.

use std::ffi::{OsStr, OsString};
```


- [ ] **Step 2: Port the moved tests and add the two N1 tests**

In `crates/uw-host/tests/discovery.rs` (moved): switch the imports to `uw_host`, drop the three `driver_*` tests (they move to `tests/driver.rs` in Step 3, rewritten), and add the explicit-pid test. Make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use std::path::PathBuf;

use uw_inject::driver::Driver;
use uw_inject::{exe_matches, to_wine_path};
use uw_pipe_proto::jsonl::{Command, Event};

#[cfg(unix)]
use uw_inject::discovery::{
    cmdline_names, config_info_for_prefix, find_pids, proton_files_from_config_info, server_socket, wine_env,
    GameError, GameSession,
```

with:

```rust
use std::path::PathBuf;

use uw_host::{exe_matches, to_wine_path};

#[cfg(unix)]
use uw_host::discovery::{
    cmdline_names, config_info_for_prefix, find_pids, proton_files_from_config_info, server_socket, wine_env,
    GameError, GameSession,
```

2. Replace:

```rust
    assert!(err.to_string().contains("uw-no-such-game.exe is not running"), "{err}");
}

#[cfg(unix)]
#[test]
fn game_error_codes_match_the_design() {
```

with:

```rust
    assert!(err.to_string().contains("uw-no-such-game.exe is not running"), "{err}");
}

/// Catches an explicit `--pid` that does not exist being reported as `no_wine_session` (plan 1
/// Task 3 review M2; its own test per N1). `pid_max` is far below `i32::MAX`.
#[cfg(unix)]
#[test]
fn an_explicit_pid_that_does_not_exist_is_game_not_found() {
    let err = GameSession::find("x.exe", Some(i32::MAX)).unwrap_err();
    assert_eq!(err.code(), "game_not_found", "{err}");
}

#[cfg(unix)]
#[test]
fn game_error_codes_match_the_design() {
```

3. Replace:

```rust
    assert!(exe_matches(r"S:\common\Deadlock\game\bin\win64\deadlock.exe", "deadlock.exe"));
    assert!(!exe_matches("deadlock.exe.bak", "deadlock.exe"));
}

#[test]
fn driver_loads_overlay_once_when_bootstrap_connects_fresh() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Fresh connection with no overlay: send load_overlay once.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }),
        vec![Command::LoadOverlay { request_id: 1, path: r"Z:\games\overlay.dll".into() }]
    );
    // A later status does not re-send.
    assert_eq!(
        d.on_event(&Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() }),
        vec![]
    );
    assert!(d.is_done());
}

#[test]
fn driver_does_not_reload_after_client_restart() {
    let mut d = Driver::new(Some(r"Z:\games\overlay.dll".into()));
    // Reconnect: the pinned bootstrap already has the overlay, so send nothing.
    assert_eq!(
        d.on_event(&Event::BootstrapConnected { overlay_loaded: true, overlay_path: Some(r"Z:\games\overlay.dll".into()) }),
        vec![]
    );
    assert!(d.is_done());
}

#[test]
fn driver_with_no_overlay_sends_nothing() {
    let mut d = Driver::new(None);
    assert_eq!(d.on_event(&Event::BootstrapConnected { overlay_loaded: false, overlay_path: None }), vec![]);
}
```

with:

```rust
    assert!(exe_matches(r"S:\common\Deadlock\game\bin\win64\deadlock.exe", "deadlock.exe"));
    assert!(!exe_matches("deadlock.exe.bak", "deadlock.exe"));
}
```


In `crates/uw-host/tests/cli.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use std::path::{Path, PathBuf};

use uw_inject::discovery::{helper_command, GameSession};

fn session() -> GameSession {
```

with:

```rust
use std::path::{Path, PathBuf};

use uw_host::discovery::{helper_command, GameSession};

fn session() -> GameSession {
```

2. Replace:

```rust
    assert_eq!(args.last(), Some(&OsStr::new("--fallback")));
}
```

with:

```rust
    assert_eq!(args.last(), Some(&OsStr::new("--fallback")));
}

/// Catches a sync variable from the caller's shell leaking into the helper when the game does not
/// set it (plan 1 Task 3 review M3; its own test per N1): the helper's sync mode must match the
/// running wineserver.
#[test]
fn helper_command_clears_sync_variables_the_game_does_not_set() {
    let cmd = helper_command(
        &session(),
        Path::new("/x/uw-helper64.exe"),
        "deadlock.exe",
        Path::new("/x/uw_bootstrap64.dll"),
        false,
    );
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("WINEESYNC"), None)), "{envs:?}");
    assert!(envs.contains(&(OsStr::new("WINENTSYNC"), None)), "{envs:?}");
    assert!(envs.contains(&(OsStr::new("WINEFSYNC"), Some(OsStr::new("1")))), "{envs:?}");
}
```


- [ ] **Step 3: Write the failing driver, supervisor and runtime tests**

`crates/uw-host/tests/driver.rs` (complete file):

```rust
use uw_host::driver::{Desired, Driver};
use uw_host::RequestIds;
use uw_pipe_proto::jsonl::{Command, Event};

const OVERLAY: &str = r"Z:\games\overlay.dll";

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
}

/// Catches a fresh bootstrap left without the overlay, or a status that triggers another load.
#[test]
fn a_fresh_bootstrap_gets_load_overlay_and_a_status_sends_nothing() {
    let mut d = Driver::new(Desired::new(Some(OVERLAY.into())), RequestIds::default());
    assert_eq!(
        d.on_event(&bootstrap_connected(false)),
        vec![Command::LoadOverlay { request_id: 1, path: OVERLAY.into() }]
    );
    let ok = Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() };
    assert_eq!(d.on_event(&ok), vec![]);
}

/// Catches a reload after a client restart: the pinned bootstrap already runs the overlay.
#[test]
fn a_bootstrap_that_has_the_overlay_gets_no_load() {
    let mut d = Driver::new(Desired::new(Some(OVERLAY.into())), RequestIds::default());
    assert_eq!(d.on_event(&bootstrap_connected(true)), vec![]);
}

#[test]
fn with_no_overlay_nothing_is_loaded() {
    let mut d = Driver::new(Desired::new(None), RequestIds::default());
    assert_eq!(d.on_event(&bootstrap_connected(false)), vec![]);
}

/// Catches an overlay connect without the full state, or with stale values, or reusing an id the
/// user already took.
#[test]
fn every_overlay_connect_gets_the_full_desired_state_with_a_fresh_id() {
    let ids = RequestIds::default();
    let mut d = Driver::new(Desired::new(None), ids.clone());
    let connected = Event::OverlayConnected { module_version: 1 };
    assert_eq!(
        d.on_event(&connected),
        vec![Command::SetState { request_id: 1, edit_mode: Some(false), visible: Some(true) }]
    );
    d.note_command(&Command::SetState { request_id: ids.next(), edit_mode: Some(true), visible: None });
    assert_eq!(
        d.on_event(&connected),
        vec![Command::SetState { request_id: 3, edit_mode: Some(true), visible: Some(true) }]
    );
    d.note_command(&Command::LoadOverlay { request_id: ids.next(), path: r"Z:\b.dll".into() });
    assert_eq!(d.desired(), &Desired { overlay: Some(r"Z:\b.dll".into()), edit_mode: true, visible: true });
}
```

`crates/uw-host/tests/supervisor.rs` (complete file):

```rust
//! The supervisor against a scripted fake helper on a fake clock: the test plays the helper by
//! feeding its stdout lines and exits, and checks what the host would send and emit.

use uw_host::driver::Desired;
use uw_host::supervisor::{Action, Input, Supervisor, OVERLAY_TIMEOUT_MS};
use uw_host::RequestIds;
use uw_pipe_proto::jsonl::{Command, Event};

const OVERLAY: &str = r"Z:\games\overlay.dll";

struct Fake {
    sup: Supervisor,
    ids: RequestIds,
    now: u64,
}

impl Fake {
    /// A started host with `OVERLAY` desired.
    fn started() -> Fake {
        let ids = RequestIds::default();
        let mut sup = Supervisor::new(Desired::new(Some(OVERLAY.into())), ids.clone());
        assert_eq!(sup.start(0), vec![Action::StartHelper]);
        Fake { sup, ids, now: 0 }
    }
    /// The fake helper prints `event`.
    fn says(&mut self, event: Event) -> Vec<Action> {
        self.sup.handle(Input::HelperLine(event.to_line()), self.now)
    }
    fn exits(&mut self) -> Vec<Action> {
        self.sup.handle(Input::HelperExited, self.now)
    }
    fn user(&mut self, cmd: Command) -> Vec<Action> {
        self.sup.handle(Input::User(cmd), self.now)
    }
    fn at(&mut self, ms: u64) -> Vec<Action> {
        self.now = ms;
        self.sup.tick(ms)
    }
}

fn emit(e: Event) -> Action {
    Action::Emit(e)
}

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
}

/// Catches reconcile commands reusing a user's id, a reply not clearing its command, or a dead
/// helper leaving a command unanswered (or answering one twice).
#[test]
fn request_ids_are_shared_and_replies_clear_their_commands() {
    let mut f = Fake::started();
    let ping = Command::Ping { request_id: f.ids.next() }; // 1
    assert_eq!(f.user(ping.clone()), vec![Action::Send(ping)]);
    // The reconcile load takes the next id from the same counter.
    assert_eq!(
        f.says(bootstrap_connected(false)),
        vec![emit(bootstrap_connected(false)), Action::Send(Command::LoadOverlay { request_id: 2, path: OVERLAY.into() })]
    );
    let pong = Event::Pong { request_id: 1, seen_seq: 0 };
    assert_eq!(f.says(pong.clone()), vec![emit(pong)]);
    // The helper dies: only the unanswered load (2) fails, once.
    let actions = f.exits();
    assert_eq!(actions, vec![emit(Event::reply_error(2, "bootstrap_not_connected"))]);
    // A command while no helper runs fails at once and is not sent.
    let set = Command::SetState { request_id: f.ids.next(), edit_mode: Some(true), visible: None };
    assert_eq!(f.user(set), vec![emit(Event::reply_error(3, "overlay_not_connected"))]);
}

/// Catches a wrong back-off (1, 2, 4, 8 s, then every 10 s), a delay that never resets after a
/// helper stayed up 60 s, or one that resets earlier (or counts from the last exit, not the start).
#[test]
fn restarts_back_off_and_reset_after_a_minute_of_uptime() {
    let mut f = Fake::started();
    let mut t = 0;
    for (attempt, delay) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (5, 10_000), (6, 10_000)] {
        t += 100;
        f.now = t;
        assert_eq!(f.exits(), vec![]);
        assert_eq!(f.sup.next_deadline(), Some(t + delay), "attempt {attempt}");
        assert_eq!(f.at(t + delay - 1), vec![], "too early for attempt {attempt}");
        t += delay;
        assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt }), Action::StartHelper]);
    }
    // This helper stays up 1 ms short of a minute: no reset, the delay stays at 10 s.
    t += 59_999;
    f.now = t;
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 10_000));
    t += 10_000;
    assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt: 7 }), Action::StartHelper]);
    // This one stays up a minute: the next delay is the first one again.
    t += 60_000;
    f.now = t;
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 1_000));
    assert_eq!(f.at(t + 1_000), vec![emit(Event::HelperRestarted { attempt: 1 }), Action::StartHelper]);
}

/// Catches a reconnect that reloads a loaded overlay, skips the full state push, or loses a
/// change made while the helper was down.
#[test]
fn reconcile_after_reconnect_loads_only_when_needed_and_pushes_the_desired_state() {
    let mut f = Fake::started();
    let on = Command::SetState { request_id: f.ids.next(), edit_mode: Some(true), visible: None }; // 1
    assert_eq!(f.user(on.clone()), vec![Action::Send(on)]);
    let connected = Event::OverlayConnected { module_version: 1 };
    assert_eq!(
        f.says(connected.clone()),
        vec![
            emit(connected.clone()),
            Action::Send(Command::SetState { request_id: 2, edit_mode: Some(true), visible: Some(true) })
        ]
    );

    // The helper dies. While it is down the user turns edit mode off: refused, but remembered.
    f.now = 5_000;
    f.exits();
    let off = Command::SetState { request_id: f.ids.next(), edit_mode: Some(false), visible: None }; // 3
    assert_eq!(f.user(off), vec![emit(Event::reply_error(3, "overlay_not_connected"))]);
    assert!(!f.sup.desired().edit_mode);

    // Restarted: the pinned bootstrap still runs the overlay, so no load; the overlay
    // reconnects and gets the full desired state, with edit mode off.
    f.at(6_000);
    assert_eq!(f.says(bootstrap_connected(true)), vec![emit(bootstrap_connected(true))]);
    assert_eq!(
        f.says(connected.clone()),
        vec![emit(connected), Action::Send(Command::SetState { request_id: 4, edit_mode: Some(false), visible: Some(true) })]
    );

    // A bootstrap that lost its overlay (a fresh game) is told to load it again.
    assert_eq!(
        f.says(bootstrap_connected(false)),
        vec![emit(bootstrap_connected(false)), Action::Send(Command::LoadOverlay { request_id: 5, path: OVERLAY.into() })]
    );
}

/// Catches a restart loop after `game_exited`, `already_running`, `version_mismatch`,
/// `hook_failed`, `bootstrap_timeout` or a failed start (design: reported once, then `stopped`,
/// no restart).
#[test]
fn the_stop_rules_end_the_host_without_a_restart() {
    for (event, reason) in [
        (Event::GameExited, "game_exited"),
        (Event::error("already_running"), "already_running"),
        (Event::error("version_mismatch"), "version_mismatch"),
        (Event::Error { request_id: None, code: "hook_failed".into(), detail: Some(1428) }, "hook_failed"),
        (Event::error("bootstrap_timeout"), "bootstrap_timeout"),
    ] {
        let mut f = Fake::started();
        assert_eq!(
            f.says(event.clone()),
            vec![emit(event), emit(Event::Stopped { reason: reason.into() }), Action::Stop],
            "{reason}"
        );
        assert_eq!(f.exits(), vec![], "{reason}");
        assert_eq!(f.sup.next_deadline(), None, "{reason}: no restart scheduled");
        assert!(f.sup.is_stopped());
    }

    let mut f = Fake::started();
    f.exits();
    f.at(1_000);
    assert_eq!(
        f.sup.handle(Input::StartFailed("game_not_found".into()), 1_000),
        vec![emit(Event::error("game_not_found")), emit(Event::Stopped { reason: "game_not_found".into() }), Action::Stop]
    );
    assert_eq!(f.sup.next_deadline(), None);
}

/// Catches the other helper failures stopping the host: the design restarts the helper.
#[test]
fn other_helper_errors_are_reported_and_the_helper_restarted() {
    for code in ["no_window", "channel_failed", "pipe_error"] {
        let mut f = Fake::started();
        assert_eq!(f.says(Event::error(code)), vec![emit(Event::error(code))], "{code}");
        f.now = 100;
        f.exits();
        assert_eq!(f.at(1_100), vec![emit(Event::HelperRestarted { attempt: 1 }), Action::StartHelper], "{code}");
    }
}

/// Catches an unparseable helper line being dropped silently (plan 1 deferred minor).
#[test]
fn a_line_that_is_not_an_event_is_a_helper_protocol_error() {
    let mut f = Fake::started();
    assert_eq!(
        f.sup.handle(Input::HelperLine("wine: something odd".into()), 0),
        vec![emit(Event::HelperProtocolError { line: "wine: something odd".into() })]
    );
}

/// Catches `overlay_timeout` missing, repeated, or raised although the overlay connected.
#[test]
fn status_ok_without_an_overlay_within_10_s_is_overlay_timeout() {
    let ok = |id| Event::Status { request_id: id, code: "ok".into(), detail: 0, message: String::new() };
    let mut f = Fake::started();
    f.now = 1_000;
    f.says(ok(1));
    assert_eq!(f.at(1_000 + OVERLAY_TIMEOUT_MS - 1), vec![]);
    assert_eq!(f.at(1_000 + OVERLAY_TIMEOUT_MS), vec![emit(Event::OverlayTimeout)]);
    assert_eq!(f.at(60_000), vec![], "reported once");

    let mut f = Fake::started();
    f.says(ok(1));
    f.now = 5_000;
    f.says(Event::OverlayConnected { module_version: 1 });
    assert_eq!(f.at(30_000), vec![]);
}

#[test]
fn shutdown_stops_quietly() {
    let mut f = Fake::started();
    assert_eq!(f.sup.handle(Input::Shutdown, 0), vec![Action::Stop]);
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), None);
}
```

`crates/uw-host/tests/host.rs` (complete file):

```rust
#![cfg(unix)]
//! The real runtime against fake helpers: small `sh` scripts that print events and read commands,
//! started through a scripted `Launch`. They check the process wiring (stdin, stdout, exit, a real
//! restart, stop on close) that the supervisor's unit tests cannot.

use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use uw_host::driver::Desired;
use uw_host::host::{Host, Launch};
use uw_pipe_proto::jsonl::{Command as HelperCommand, Event};

/// Starts `sh -c <script>` for each start, in order; past the last one, `game_not_found`.
struct Scripts(Vec<&'static str>);

impl Launch for Scripts {
    fn command(&mut self) -> Result<Command, String> {
        if self.0.is_empty() {
            return Err("game_not_found".into());
        }
        let mut c = Command::new("sh");
        c.arg("-c").arg(self.0.remove(0));
        Ok(c)
    }
}

fn next(host: &Host) -> Event {
    host.recv_timeout(Duration::from_secs(10)).expect("an event within 10 s")
}

/// Catches commands not reaching the helper's stdin, its stdout not being read, a dead helper not
/// restarted, or a stop that does not close stdin (the second script waits on it).
#[test]
fn a_fake_helper_is_driven_restarted_and_stopped() {
    let host = Host::start(
        Scripts(vec![
            // Connect, echo the first command back (not an event: a protocol error), then die.
            r#"echo '{"event":"bootstrap_connected","overlay_loaded":false}'; read -r line; printf 'got %s\n' "$line"; exit 1"#,
            // Report the game gone, then wait for stdin to close.
            r#"echo '{"event":"game_exited"}'; read -r line; exit 0"#,
        ]),
        Desired::new(Some(r"Z:\o.dll".into())),
    );
    assert_eq!(next(&host), Event::BootstrapConnected { overlay_loaded: false, overlay_path: None });
    assert_eq!(
        next(&host),
        Event::HelperProtocolError { line: r#"got {"cmd":"load_overlay","request_id":1,"path":"Z:\\o.dll"}"#.into() }
    );
    assert_eq!(next(&host), Event::reply_error(1, "bootstrap_not_connected"));
    assert_eq!(next(&host), Event::HelperRestarted { attempt: 1 });
    assert_eq!(next(&host), Event::GameExited);
    assert_eq!(next(&host), Event::Stopped { reason: "game_exited".into() });
    // The host closed the script's stdin, it exited, and the host ended.
    assert_eq!(host.recv_timeout(Duration::from_secs(10)), Err(RecvTimeoutError::Disconnected));
}

/// Catches a user command losing its id on the way, or a reply not reaching the user.
#[test]
fn a_user_command_and_its_reply_keep_their_request_id() {
    let host = Host::start(
        Scripts(vec![
            r#"read -r line; id=$(echo "$line" | sed 's/.*"request_id":\([0-9]*\).*/\1/'); echo "{\"event\":\"pong\",\"request_id\":$id,\"seen_seq\":4}"; read rest"#,
        ]),
        Desired::new(None),
    );
    let id = host.next_request_id();
    host.send(HelperCommand::Ping { request_id: id });
    assert_eq!(next(&host), Event::Pong { request_id: id, seen_seq: 4 });
    // Dropping the host closes the script's stdin; it must not be left running.
}

/// Catches a failed start that loops instead of stopping.
#[test]
fn a_helper_that_cannot_start_stops_the_host() {
    let host = Host::start(Scripts(vec![]), Desired::new(None));
    assert_eq!(next(&host), Event::error("game_not_found"));
    assert_eq!(next(&host), Event::Stopped { reason: "game_not_found".into() });
    assert_eq!(host.recv_timeout(Duration::from_secs(5)), Err(RecvTimeoutError::Disconnected));
}
```

- [ ] **Step 4: Run them to see them fail**

Run: `cargo test -p uw-host`
Expected: does not compile: error E0583, file not found for module `supervisor` (and the same for `host`), and the tests' imports of `uw_host::driver::Desired`, `uw_host::supervisor` and `uw_host::host` are unresolved (the moved `driver.rs` is still plan 1's).

- [ ] **Step 5: Write the driver, the supervisor and the runtime**

`crates/uw-host/src/driver.rs` (complete file, replacing the moved plan 1 driver):

```rust
//! The reconcile policy (design 2a "Components and flow"): the host keeps a desired state
//! (overlay path, `edit_mode`, `visible`) and, after every (re)connect, sends what makes the game
//! match it: `load_overlay` when `bootstrap_connected` says no overlay is loaded, and a full
//! `set_state` on every `overlay_connected`. A user's command updates the desired state as it is
//! sent, so a command that fails while the overlay is away still lands on the next connect.

use uw_pipe_proto::jsonl::{Command, Event};

use crate::RequestIds;

/// What the host wants the game to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    /// The overlay's Windows path; `None` to connect only.
    pub overlay: Option<String>,
    pub edit_mode: bool,
    pub visible: bool,
}

impl Desired {
    /// The overlay to load, visible and not in edit mode.
    pub fn new(overlay: Option<String>) -> Desired {
        Desired { overlay, edit_mode: false, visible: true }
    }
}

pub struct Driver {
    desired: Desired,
    ids: RequestIds,
}

impl Driver {
    /// `ids` is shared with the host's user, so reconcile commands never reuse a user's id.
    pub fn new(desired: Desired, ids: RequestIds) -> Driver {
        Driver { desired, ids }
    }

    pub fn desired(&self) -> &Desired {
        &self.desired
    }

    /// A user's command is about to be sent: record what it asks for.
    pub fn note_command(&mut self, cmd: &Command) {
        match cmd {
            Command::LoadOverlay { path, .. } => self.desired.overlay = Some(path.clone()),
            Command::SetState { edit_mode, visible, .. } => {
                if let Some(on) = edit_mode {
                    self.desired.edit_mode = *on;
                }
                if let Some(on) = visible {
                    self.desired.visible = *on;
                }
            }
            Command::Ping { .. } => {}
        }
    }

    /// A helper event: the commands that bring the game to the desired state.
    pub fn on_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::BootstrapConnected { overlay_loaded: false, .. } => match &self.desired.overlay {
                Some(path) => vec![Command::LoadOverlay { request_id: self.ids.next(), path: path.clone() }],
                None => vec![],
            },
            Event::OverlayConnected { .. } => vec![Command::SetState {
                request_id: self.ids.next(),
                edit_mode: Some(self.desired.edit_mode),
                visible: Some(self.desired.visible),
            }],
            _ => vec![],
        }
    }
}
```

`crates/uw-host/src/supervisor.rs` (complete file):

```rust
//! The host's supervision rules (design 2a "Failures"), as a pure state machine on a millisecond
//! clock, so they are unit-tested with a scripted fake helper:
//! - every command to the helper is tracked by its request id until its reply; when the helper
//!   exits, or none is running, the command fails with `bootstrap_not_connected` (`load_overlay`)
//!   or `overlay_not_connected` (`set_state`, `ping`) — no queueing while disconnected;
//! - a helper that exits while the game runs is restarted after 1, 2, 4, 8 s, then every 10 s;
//!   the delay resets once a helper has been up 60 s (`helper_restarted {attempt}`);
//! - `game_exited`, `already_running`, `version_mismatch`, `hook_failed`, `bootstrap_timeout` and
//!   a failed start stop the host for good (`stopped {reason}`), with no restart;
//! - `status ok` without `overlay_connected` within 10 s is `overlay_timeout`;
//! - a stdout line that is not an event is `helper_protocol_error {line}`;
//! - after every (re)connect the `Driver` reconciles the desired state.

use uw_pipe_proto::jsonl::{Command, Event};

use crate::driver::{Desired, Driver};
use crate::RequestIds;

/// `status ok` -> `overlay_connected` deadline.
pub const OVERLAY_TIMEOUT_MS: u64 = 10_000;
/// Restart delays for the first four restarts; later ones wait `RESTART_DELAY_MAX_MS`.
pub const RESTART_DELAYS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];
pub const RESTART_DELAY_MAX_MS: u64 = 10_000;
/// A helper up this long resets the restart delay to the first one.
pub const UPTIME_RESET_MS: u64 = 60_000;
/// Helper errors that stop the host instead of a restart (design "Failures"): a restart would
/// only fail again, and for the hook errors would set a new hook on the game's UI thread.
pub const STOP_CODES: [&str; 4] = ["already_running", "version_mismatch", "hook_failed", "bootstrap_timeout"];

/// Something that happened to the host. Time passing is `Supervisor::tick`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A line the current helper wrote on stdout.
    HelperLine(String),
    /// The current helper exited (its stdout closed and it was reaped).
    HelperExited,
    /// A helper could not be started; the error code (`game_not_found`, `no_wine_session`,
    /// `helper_start_failed`).
    StartFailed(String),
    /// A command from the host's user, request id already assigned.
    User(Command),
    /// The host's user is going away: stop, without an event.
    Shutdown,
}

/// What the runtime must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start a helper. The runtime later reports `HelperExited`, or `StartFailed` at once.
    StartHelper,
    /// Write this command to the current helper's stdin.
    Send(Command),
    /// Hand this event to the host's user.
    Emit(Event),
    /// Close the helper's stdin (it exits), and end once it has. Nothing restarts after this.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// A helper runs, started at this time.
    Running { since: u64 },
    /// No helper; the next one starts at this time.
    Waiting { restart_at: u64 },
    Stopped,
}

pub struct Supervisor {
    driver: Driver,
    phase: Phase,
    /// Restarts since the last reset.
    attempt: u32,
    /// Commands sent to the current helper and not answered yet.
    pending: Vec<Command>,
    overlay_up: bool,
    overlay_deadline: Option<u64>,
}

/// The error a command gets when the side it needs is not there.
fn not_connected(cmd: &Command) -> Event {
    let code = match cmd {
        Command::LoadOverlay { .. } => "bootstrap_not_connected",
        Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
    };
    Event::reply_error(cmd.request_id(), code)
}

impl Supervisor {
    /// `ids` is the counter the host's user also takes ids from.
    pub fn new(desired: Desired, ids: RequestIds) -> Supervisor {
        Supervisor {
            driver: Driver::new(desired, ids),
            phase: Phase::Stopped,
            attempt: 0,
            pending: Vec::new(),
            overlay_up: false,
            overlay_deadline: None,
        }
    }

    /// The first helper.
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        self.phase = Phase::Running { since: now };
        vec![Action::StartHelper]
    }

    pub fn desired(&self) -> &Desired {
        self.driver.desired()
    }

    pub fn is_stopped(&self) -> bool {
        self.phase == Phase::Stopped
    }

    /// When `tick` next has something to do.
    pub fn next_deadline(&self) -> Option<u64> {
        let restart = match self.phase {
            Phase::Waiting { restart_at } => Some(restart_at),
            _ => None,
        };
        [restart, self.overlay_deadline].into_iter().flatten().min()
    }

    pub fn handle(&mut self, input: Input, now: u64) -> Vec<Action> {
        match input {
            Input::HelperLine(line) => match Event::parse(&line) {
                Ok(event) => self.on_event(event, now),
                Err(_) => vec![Action::Emit(Event::HelperProtocolError { line })],
            },
            Input::HelperExited => self.on_exit(now),
            Input::StartFailed(code) => {
                let mut out = vec![Action::Emit(Event::error(&code))];
                out.extend(self.stop(&code));
                out
            }
            Input::User(cmd) => {
                self.driver.note_command(&cmd);
                self.send(cmd)
            }
            Input::Shutdown => {
                if self.is_stopped() {
                    vec![]
                } else {
                    self.phase = Phase::Stopped;
                    self.overlay_deadline = None;
                    vec![Action::Stop]
                }
            }
        }
    }

    /// Restart a helper whose delay is over; report an overdue overlay.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if let Phase::Waiting { restart_at } = self.phase
            && now >= restart_at
        {
            self.phase = Phase::Running { since: now };
            out.push(Action::Emit(Event::HelperRestarted { attempt: self.attempt }));
            out.push(Action::StartHelper);
        }
        if self.overlay_deadline.is_some_and(|d| now >= d) {
            self.overlay_deadline = None;
            out.push(Action::Emit(Event::OverlayTimeout));
        }
        out
    }

    fn send(&mut self, cmd: Command) -> Vec<Action> {
        if matches!(self.phase, Phase::Running { .. }) {
            self.pending.push(cmd.clone());
            vec![Action::Send(cmd)]
        } else {
            vec![Action::Emit(not_connected(&cmd))]
        }
    }

    fn stop(&mut self, reason: &str) -> Vec<Action> {
        if self.is_stopped() {
            return vec![];
        }
        self.phase = Phase::Stopped;
        self.overlay_deadline = None;
        vec![Action::Emit(Event::Stopped { reason: reason.to_string() }), Action::Stop]
    }

    fn on_event(&mut self, event: Event, now: u64) -> Vec<Action> {
        if let Some(id) = event.request_id() {
            self.pending.retain(|c| c.request_id() != id);
        }
        match &event {
            Event::Status { code, .. } if code == "ok" && !self.overlay_up => {
                self.overlay_deadline = Some(now + OVERLAY_TIMEOUT_MS);
            }
            Event::OverlayConnected { .. } => {
                self.overlay_up = true;
                self.overlay_deadline = None;
            }
            Event::OverlayDisconnected => self.overlay_up = false,
            _ => {}
        }
        let mut out = vec![Action::Emit(event.clone())];
        match &event {
            Event::GameExited => out.extend(self.stop("game_exited")),
            Event::Error { code, .. } if STOP_CODES.contains(&code.as_str()) => {
                out.extend(self.stop(code));
            }
            _ if !self.is_stopped() => {
                for cmd in self.driver.on_event(&event) {
                    out.extend(self.send(cmd));
                }
            }
            _ => {}
        }
        out
    }

    fn on_exit(&mut self, now: u64) -> Vec<Action> {
        let out: Vec<Action> = self.pending.drain(..).map(|c| Action::Emit(not_connected(&c))).collect();
        self.overlay_up = false;
        self.overlay_deadline = None;
        if let Phase::Running { since } = self.phase {
            if now.saturating_sub(since) >= UPTIME_RESET_MS {
                self.attempt = 0;
            }
            self.attempt += 1;
            let delay = RESTART_DELAYS_MS.get(self.attempt as usize - 1).copied().unwrap_or(RESTART_DELAY_MAX_MS);
            self.phase = Phase::Waiting { restart_at: now + delay };
        }
        out
    }
}
```

`crates/uw-host/src/host.rs` (complete file):

```rust
//! The running host. One thread owns the `Supervisor`: it starts helpers through a `Launch`,
//! writes commands to the current helper's stdin, and gets the helper's stdout lines from a reader
//! thread. Events come out of `Host::recv` in order; it returns `None` once the host has stopped
//! and its helper has exited. Dropping the `Host` stops it (the helper's stdin closes, so it
//! exits: design "Failures", "uw-host exits or dies").

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use uw_pipe_proto::jsonl::{Command as HelperCommand, Event};

use crate::discovery::{helper_command, GameSession};
use crate::driver::Desired;
use crate::supervisor::{Action, Input, Supervisor};
use crate::RequestIds;

/// How the host starts one helper process.
pub trait Launch: Send + 'static {
    /// The helper's command line (the host pipes its stdin and stdout), or the error code
    /// (`game_not_found`, `no_wine_session`) when the game or its Wine session is gone.
    fn command(&mut self) -> Result<Command, String>;
}

/// `uw-helper64.exe` in the game's Wine session. Every start finds the game again by its pid, so
/// a restart after the game has gone stops with `game_not_found`.
pub struct WineLaunch {
    pub session: GameSession,
    pub exe: String,
    pub helper: PathBuf,
    pub bootstrap: PathBuf,
    pub fallback: bool,
}

impl Launch for WineLaunch {
    fn command(&mut self) -> Result<Command, String> {
        let session =
            GameSession::find_with_proton(&self.exe, Some(self.session.pid), Some(self.session.proton.clone()))
                .map_err(|e| {
                    eprintln!("[uw-host] {e}");
                    e.code().to_string()
                })?;
        let mut cmd = helper_command(&session, &self.helper, &self.exe, &self.bootstrap, self.fallback);
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
        }
        Ok(cmd)
    }
}

pub struct Host {
    inputs: Sender<Input>,
    events: Receiver<Event>,
    ids: RequestIds,
}

impl Host {
    /// Start supervising; the first helper starts at once.
    pub fn start(launch: impl Launch, desired: Desired) -> Host {
        let ids = RequestIds::default();
        let (inputs, rx) = channel();
        let (etx, events) = channel();
        let supervisor = Supervisor::new(desired, ids.clone());
        let tx = inputs.clone();
        std::thread::Builder::new()
            .name("uw-host".into())
            .spawn(move || run(supervisor, launch, rx, tx, etx))
            .expect("spawn the host thread");
        Host { inputs, events, ids }
    }

    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// Send a command (it updates the desired state, then goes to the helper, or fails at once
    /// with `*_not_connected` while none runs). Its reply arrives as an event.
    pub fn send(&self, cmd: HelperCommand) {
        let _ = self.inputs.send(Input::User(cmd));
    }

    /// The next event; `None` once the host has stopped and its helper has exited.
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }

    pub fn recv_timeout(&self, d: Duration) -> Result<Event, RecvTimeoutError> {
        self.events.recv_timeout(d)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.inputs.send(Input::Shutdown);
    }
}

/// Spawn a helper and its stdout reader. The reader sends each non-empty line, then
/// `HelperExited` once stdout closes and the helper is reaped.
fn spawn(mut cmd: Command, tx: &Sender<Input>) -> Result<ChildStdin, String> {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| {
        eprintln!("[uw-host] starting the helper: {e}");
        "helper_start_failed".to_string()
    })?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let tx = tx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() && tx.send(Input::HelperLine(line)).is_err() {
                break;
            }
        }
        let _ = child.wait();
        let _ = tx.send(Input::HelperExited);
    });
    Ok(stdin)
}

fn run(mut sup: Supervisor, mut launch: impl Launch, rx: Receiver<Input>, tx: Sender<Input>, events: Sender<Event>) {
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_millis() as u64;
    let mut stdin: Option<ChildStdin> = None;
    let mut helper_running = false;
    let mut stopping = false;
    let mut actions = sup.start(now());
    loop {
        let mut i = 0;
        while i < actions.len() {
            match actions[i].clone() {
                Action::StartHelper => match launch.command().and_then(|cmd| spawn(cmd, &tx)) {
                    Ok(s) => {
                        stdin = Some(s);
                        helper_running = true;
                    }
                    Err(code) => actions.extend(sup.handle(Input::StartFailed(code), now())),
                },
                Action::Send(cmd) => {
                    if let Some(s) = stdin.as_mut() {
                        // A write that fails means the helper is exiting; its `HelperExited`
                        // fails the command then.
                        let _ = writeln!(s, "{}", cmd.to_line()).and_then(|_| s.flush());
                    }
                }
                Action::Emit(e) => {
                    let _ = events.send(e);
                }
                Action::Stop => {
                    stopping = true;
                    stdin = None; // EOF: the helper exits
                }
            }
            i += 1;
        }
        actions.clear();
        if stopping && !helper_running {
            return; // dropping `events` ends `Host::recv`
        }
        let input = match sup.next_deadline() {
            Some(at) => match rx.recv_timeout(Duration::from_millis(at.saturating_sub(now()))) {
                Ok(input) => Some(input),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(input) => Some(input),
                Err(_) => return,
            },
        };
        if let Some(input) = input {
            if input == Input::HelperExited {
                helper_running = false;
                stdin = None;
            }
            actions.extend(sup.handle(input, now()));
        }
        actions.extend(sup.tick(now()));
    }
}
```

- [ ] **Step 6: Point `uw-inject` at `uw-host`**

`crates/uw-inject/Cargo.toml` (complete file):

```toml
[package]
name = "uw-inject"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# The operator CLI (`uw-inject`): find a running Proton game, start `uw-helper64.exe` in its Wine
# session and drive it over JSON lines. Discovery and driving live in `uw-host`.
[dependencies]
uw-host = { path = "../uw-host" }
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

In `crates/uw-inject/src/bin/uw-inject.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
    use std::process::Stdio;

    use uw_inject::discovery::{helper_command, GameSession};
    use uw_inject::driver::Driver;
    use uw_inject::to_wine_path;
    use uw_pipe_proto::jsonl::Event;
```

with:

```rust
    use std::process::Stdio;

    use uw_host::discovery::{helper_command, GameSession};
    use uw_host::driver::{Desired, Driver};
    use uw_host::{to_wine_path, RequestIds};
    use uw_pipe_proto::jsonl::Event;
```

2. Replace:

```rust
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut driver = Driver::new(overlay.as_deref().map(to_wine_path));

    for line in BufReader::new(stdout).lines() {
```

with:

```rust
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut driver = Driver::new(Desired::new(overlay.as_deref().map(to_wine_path)), RequestIds::default());

    for line in BufReader::new(stdout).lines() {
```


In `crates/uw-inject/tests/wine_e2e.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
use std::time::{Duration, Instant, SystemTime};

use uw_inject::discovery::find_pids;

fn wine() -> String {
```

with:

```rust
use std::time::{Duration, Instant, SystemTime};

use uw_host::discovery::find_pids;

fn wine() -> String {
```


- [ ] **Step 7: Run the tests and both clippies**

Run:
```bash
cargo test -p uw-host -p uw-inject
for i in 1 2 3; do cargo test -q -p uw-host --test host; done
cargo clippy -p uw-host -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-host -p uw-inject --all-targets -- -D warnings
```
Expected: `uw-host`: `cli` 3, `discovery` 14, `driver` 4, `host` 3 (about 1 s: one real 1 s restart), `supervisor` 8; `uw-inject`: its Wine test ignored (1); the repeated `host` runs pass each time; both clippies clean (on Windows the unix-only modules and tests compile out).

- [ ] **Step 8: Run the `uw-inject` Wine chain test (its binary changed)**

Run:
```bash
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: `1 passed`; no leftovers. (The Windows artifacts from Task 3 are current: no Windows code changed.)

- [ ] **Step 9: Commit**

```bash
git add -A crates/uw-host crates/uw-inject Cargo.lock
git commit -m "channel: uw-host library (discovery, reconcile, supervisor, runtime)"
```

---

## Task 5: Split `uw-helper64.rs` by job (no behaviour change)

Design "Code layout" and "Risks": plan 1's 667-line helper binary is split before it grows, with plan 1's Wine suite green before and after. This is a move: the code below is plan 1's (as Task 3 left it), cut into modules; the only edits are what the split needs (`pub(crate)`, `PipeServer` owning the pipe's handles and accept state that `RealWin32` owned, `self.now_ms()` becoming `now_ms()`, the bootstrap pipe held as `Option<PipeServer>`, `main` renamed `helper_main`).

- `main.rs`: arguments, `decide::run`, hand-over to the relay; `wide`, `wait_ms`.
- `win32.rs`: `RealWin32` and its `Win32` impl (game and window lookup, mutexes, hook, wake-ups).
- `pipe_server.rs`: the overlapped pipe server (`Accept` state machine, `read_message`, `write_message`).
- `relay.rs`: `emit`, the JSON-lines relay, the game watcher.

**Files:**
- Delete: `crates/uw-helper/src/bin/uw-helper64.rs`
- Create: `crates/uw-helper/src/bin/uw-helper64/main.rs`, `…/win32.rs`, `…/pipe_server.rs`, `…/relay.rs`
- Modify: `crates/uw-helper/Cargo.toml`

- [ ] **Step 1: Point the binary at the new directory**

In `crates/uw-helper/Cargo.toml`, make this edit (each "replace" block is exact and occurs once):

Replace:

```toml
[[bin]]
name = "uw-helper64"
path = "src/bin/uw-helper64.rs"

[dependencies]
```

with:

```toml
[[bin]]
name = "uw-helper64"
path = "src/bin/uw-helper64/main.rs"

[dependencies]
```


- [ ] **Step 2: Create the four modules and delete the old file**

`crates/uw-helper/src/bin/uw-helper64/main.rs` (complete file):

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Finds the game's UI thread and sets a thread hook that
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`, …) on stdout, commands (`load_overlay`, …) on stdin, each reply echoing its
//! command's `request_id`; diagnostics on stderr.
//!
//! Modules: `win32` (the real `Win32` for `decide`), `pipe_server` (the overlapped pipe server),
//! `relay` (the JSON-lines relay and the game watcher).

#[cfg(not(windows))]
fn main() {
    eprintln!("uw-helper64 runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
mod pipe_server;
#[cfg(windows)]
mod relay;
#[cfg(windows)]
mod win32;

#[cfg(windows)]
fn main() {
    std::process::exit(helper_main());
}

#[cfg(windows)]
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A wait timeout in ms for the Win32 wait functions, never `INFINITE`.
#[cfg(windows)]
pub(crate) fn wait_ms(ms: u64) -> u32 {
    use windows_sys::Win32::System::Threading::INFINITE;
    u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1)
}

#[cfg(windows)]
fn helper_main() -> i32 {
    use uw_helper::decide::{run, Config, Outcome};

    let mut args = std::env::args().skip(1);
    let exe = args.next().unwrap_or_default();
    let mut bootstrap = String::new();
    let mut force_fallback = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bootstrap" => bootstrap = args.next().unwrap_or_default(),
            "--fallback" => force_fallback = true,
            other => {
                eprintln!("[uw-helper64] unknown argument {other}");
                return 2;
            }
        }
    }
    if exe.is_empty() || bootstrap.is_empty() {
        eprintln!("usage: uw-helper64 <game-exe> --bootstrap <path> [--fallback]");
        return 2;
    }

    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
    // do not block for two minutes; unset in production, the defaults stand.
    let env_ms = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let defaults = Config::default();
    let cfg = Config {
        force_fallback,
        window_timeout_ms: env_ms("UW_WINDOW_TIMEOUT_MS", defaults.window_timeout_ms),
        hello_timeout_ms: env_ms("UW_HELLO_TIMEOUT_MS", defaults.hello_timeout_ms),
        window_poll_ms: defaults.window_poll_ms,
    };
    let outcome = run(&mut w, &cfg);
    relay::emit(&outcome.to_event());
    match outcome {
        Outcome::Connected { .. } => {
            let pid = w.pid;
            if let Some(pipe) = w.pipe.as_mut() {
                relay::relay(pipe, pid);
            }
            0
        }
        // The game ended before the bootstrap connected: the same report, and the same exit
        // code, as the post-connect watcher's `game_exited`.
        Outcome::GameExited => 0,
        other => {
            eprintln!("[uw-helper64] {other:?}");
            1
        }
    }
}
```

`crates/uw-helper/src/bin/uw-helper64/win32.rs` (complete file):

```rust
//! The real `Win32` for `decide`: Toolhelp and window lookups, the helper and marker mutexes, the
//! thread hook and its wake-ups, and the bootstrap pipe (a `PipeServer`).

use std::panic::{catch_unwind, AssertUnwindSafe};

use uw_helper::decide::{HelloWait, HookKind, Win32};
use uw_pipe_proto::names::{helper_mutex_name, marker_mutex_name, pipe_name};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM,
    LRESULT, RECT, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenMutexW, OpenProcess, Sleep, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, PostThreadMessageW,
    SendNotifyMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GW_OWNER, HHOOK, HOOKPROC, WH_CALLWNDPROC,
    WH_GETMESSAGE, WM_NULL,
};

use crate::pipe_server::PipeServer;
use crate::{wait_ms, wide};

fn basename_eq(name: &[u16], target: &str) -> bool {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let s = String::from_utf16_lossy(&name[..end]);
    let base = s.rsplit(['\\', '/']).next().unwrap_or(&s);
    base.eq_ignore_ascii_case(target)
}

struct Found {
    pid: u32,
    best_area: i64,
    thread: u32,
    hwnd: HWND,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
    // No Rust panic may cross this FFI boundary: catch and keep enumerating.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `lparam` points at the caller's `Found`.
        let f = unsafe { &mut *(lparam as *mut Found) };
        let mut pid = 0u32;
        // SAFETY: `hwnd` from EnumWindows; writes one u32.
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // SAFETY: querying visibility/owner/rect of an enumerated top-level window.
        unsafe {
            if pid == f.pid && IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() {
                let mut r: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut r) != 0 {
                    let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                    if area > f.best_area {
                        f.best_area = area;
                        f.thread = thread;
                        f.hwnd = hwnd;
                    }
                }
            }
        }
    }));
    1 // keep enumerating
}

pub(crate) struct RealWin32 {
    exe: String,
    bootstrap: String,
    /// The game's Windows pid, once `find_game` found it.
    pub(crate) pid: u32,
    hwnd: HWND,
    hook: HHOOK,
    module: HMODULE,
    helper_mutex: HANDLE,
    /// The bootstrap pipe, from `create_pipe` on.
    pub(crate) pipe: Option<PipeServer>,
}

impl RealWin32 {
    pub(crate) fn new(exe: String, bootstrap: String) -> RealWin32 {
        RealWin32 {
            exe,
            bootstrap,
            pid: 0,
            hwnd: std::ptr::null_mut(),
            hook: std::ptr::null_mut(),
            module: std::ptr::null_mut(),
            helper_mutex: std::ptr::null_mut(),
            pipe: None,
        }
    }

    fn load_bootstrap(&mut self) -> Option<HOOKPROC> {
        if self.module.is_null() {
            let w = wide(&self.bootstrap);
            // SAFETY: NUL-terminated path.
            self.module = unsafe { LoadLibraryW(w.as_ptr()) };
            if self.module.is_null() {
                return None;
            }
        }
        // SAFETY: the export matches the HOOKPROC ABI.
        let p = unsafe { GetProcAddress(self.module, c"uw_bootstrap_hook".as_ptr() as *const u8) }?;
        // SAFETY: transmute the resolved export (`FARPROC`'s inner fn type, named so the source
        // type is pinned) to the hook procedure type it really has.
        Some(Some(unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
            >(p)
        }))
    }
}

impl Drop for RealWin32 {
    fn drop(&mut self) {
        self.unhook();
        if !self.helper_mutex.is_null() {
            // SAFETY: closes the helper mutex handle this helper created and still owns.
            unsafe { CloseHandle(self.helper_mutex) };
        }
        // `pipe` drops after this: it cancels any pending accept and closes its handles.
    }
}

impl Win32 for RealWin32 {
    fn now_ms(&self) -> u64 {
        // SAFETY: a monotonic tick count is fine for the window/hello timeouts.
        unsafe { GetTickCount64() }
    }
    fn sleep_ms(&mut self, ms: u64) {
        // SAFETY: a plain sleep.
        unsafe { Sleep(wait_ms(ms)) };
    }
    fn find_game(&mut self) -> Option<u32> {
        // SAFETY: a process snapshot walked with a correctly sized PROCESSENTRY32W, closed on
        // every path (`run` polls, so a leak would grow every 500 ms).
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut found = None;
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                if e.th32ProcessID != GetCurrentProcessId() && basename_eq(&e.szExeFile, &self.exe) {
                    found = Some(e.th32ProcessID);
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            if let Some(pid) = found {
                self.pid = pid;
            }
            found
        }
    }
    fn find_ui_thread(&mut self, pid: u32) -> Option<u32> {
        let mut f = Found { pid, best_area: 0, thread: 0, hwnd: std::ptr::null_mut() };
        // SAFETY: the callback only writes through `&mut f`.
        unsafe { EnumWindows(Some(enum_cb), &mut f as *mut Found as LPARAM) };
        if f.thread != 0 {
            self.hwnd = f.hwnd;
            Some(f.thread)
        } else {
            None
        }
    }
    fn game_alive(&mut self, pid: u32) -> bool {
        // SAFETY: opens the game for SYNCHRONIZE, polls it without waiting, closes the handle.
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            CloseHandle(h);
            alive
        }
    }
    fn marker_exists(&mut self, pid: u32) -> bool {
        let name = wide(&marker_mutex_name(pid));
        // SAFETY: NUL-terminated name.
        let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
        if h.is_null() {
            false
        } else {
            // SAFETY: close the probe handle; the owner keeps the mutex alive.
            unsafe { CloseHandle(h) };
            true
        }
    }
    fn take_helper_lock(&mut self, pid: u32) -> bool {
        let name = wide(&helper_mutex_name(pid));
        // SAFETY: NUL-terminated name; keep the handle for the process lifetime.
        self.helper_mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: no arguments; read right after CreateMutexW.
        unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
    }
    fn create_pipe(&mut self, pid: u32) -> Result<(), String> {
        self.pipe = Some(PipeServer::create(&pipe_name(pid))?);
        Ok(())
    }
    fn set_hook(&mut self, kind: HookKind, thread: u32) -> Result<(), u32> {
        let Some(hookproc) = self.load_bootstrap() else {
            // SAFETY: no arguments.
            return Err(unsafe { GetLastError() });
        };
        let id = match kind {
            HookKind::GetMessage => WH_GETMESSAGE,
            HookKind::CallWndProc => WH_CALLWNDPROC,
        };
        // SAFETY: thread-scoped hook with a DLL-resident procedure and that DLL's module.
        let hook = unsafe { SetWindowsHookExW(id, hookproc, self.module, thread) };
        if hook.is_null() {
            // SAFETY: no arguments.
            Err(unsafe { GetLastError() })
        } else {
            self.hook = hook;
            let name = match kind {
                HookKind::GetMessage => "WH_GETMESSAGE",
                HookKind::CallWndProc => "WH_CALLWNDPROC",
            };
            eprintln!("[uw-helper64] hooked {name} thread={thread}");
            Ok(())
        }
    }
    /// Wake the UI thread so its next message pump runs the hook: posted `WM_NULL`s
    /// (`WH_GETMESSAGE`) and one sent `WM_NULL` to the main window (`WH_CALLWNDPROC` only
    /// fires on sent messages). `decide` calls this after each hook and every `REWAKE_MS`.
    fn post_wakeups(&mut self, thread: u32) {
        // SAFETY: posts WM_NULL to the UI thread and send-notifies its main window.
        unsafe {
            for _ in 0..5 {
                PostThreadMessageW(thread, WM_NULL, 0, 0);
            }
            if !self.hwnd.is_null() {
                SendNotifyMessageW(self.hwnd, WM_NULL, 0, 0);
            }
        }
    }
    fn wait_hello(&mut self, timeout_ms: u64) -> HelloWait {
        match self.pipe.as_mut() {
            Some(p) => p.accept_hello(timeout_ms),
            None => HelloWait::PipeError("no pipe".into()),
        }
    }
    fn unhook(&mut self) {
        if !self.hook.is_null() {
            // SAFETY: the bootstrap's self-pin keeps it loaded after unhooking.
            unsafe { UnhookWindowsHookEx(self.hook) };
            self.hook = std::ptr::null_mut();
        }
    }
    fn log(&self, msg: &str) {
        eprintln!("[uw-helper64] {msg}");
    }
}
```

`crates/uw-helper/src/bin/uw-helper64/pipe_server.rs` (complete file):

```rust
//! The helper's pipe server: one overlapped, message-mode, local-only duplex pipe instance
//! (`FILE_FLAG_FIRST_PIPE_INSTANCE`, one instance), the accept state machine that `decide`'s
//! `wait_hello` slices drive, and bounded overlapped reads and writes for the relay.

use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_pipe_proto::pipe::{encode, Message, HEADER_LEN, MAX_PAYLOAD};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::{wait_ms, wide};

/// How long the relay waits for one pipe write, and for the bootstrap's `status` reply.
pub(crate) const RELAY_TIMEOUT_MS: u64 = 15_000;

fn now_ms() -> u64 {
    // SAFETY: a monotonic tick count.
    unsafe { GetTickCount64() }
}

/// Where the client's accept stands between `accept_hello` slices. `decide` calls `wait_hello`
/// in slices of at most `REWAKE_MS`, so a slice that times out leaves the overlapped connect or
/// read pending on `PipeServer::ov` for the next slice: it is never cancelled mid-wait and the
/// client's connection is never dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accept {
    /// No I/O pending and no client: the next slice starts an overlapped `ConnectNamedPipe`.
    Listening,
    /// An overlapped `ConnectNamedPipe` is pending on `ov`.
    Connecting,
    /// A client is connected; an overlapped `ReadFile` of its first message is pending on
    /// `ov`, into `buf`.
    Reading,
    /// The first message was read; the pipe now belongs to the relay.
    Connected,
}

pub(crate) struct PipeServer {
    pipe: HANDLE,
    /// Manual-reset event for every overlapped operation (one at a time).
    event: HANDLE,
    /// The accept's `OVERLAPPED`, boxed so its address stays fixed while I/O is pending.
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    accept: Accept,
}

impl PipeServer {
    /// Create the pipe instance and its event. `name` is a `\\.\pipe\…` name.
    pub(crate) fn create(name: &str) -> Result<PipeServer, String> {
        let name = wide(name);
        // SAFETY: one overlapped, first-instance, message-mode, local-only duplex pipe.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (HEADER_LEN + MAX_PAYLOAD) as u32,
                (HEADER_LEN + MAX_PAYLOAD) as u32,
                0,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            // SAFETY: no arguments.
            return Err(format!("CreateNamedPipeW: {}", unsafe { GetLastError() }));
        }
        // SAFETY: a manual-reset, initially unsignaled event for overlapped I/O.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            // SAFETY: closes the pipe just created.
            unsafe { CloseHandle(pipe) };
            return Err("CreateEventW failed".into());
        }
        Ok(PipeServer {
            pipe,
            event,
            // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            accept: Accept::Listening,
        })
    }

    /// Cancel a pending overlapped op and wait for the cancel to finish, so its `OVERLAPPED`
    /// (and buffer) are safe to reuse or drop (plan-review I5).
    fn cancel(&self, ov: *const OVERLAPPED) {
        // SAFETY: cancels I/O on our pipe and blocks until the cancel completes.
        unsafe {
            CancelIoEx(self.pipe, ov);
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, ov, &mut n, 1);
        }
    }

    /// Free the pipe instance after a client left before its first message (the bootstrap
    /// reconnects within a second), so the next slice can accept again.
    fn drop_client(&mut self) {
        // SAFETY: disconnects our pipe instance; no I/O is pending on it here.
        unsafe { DisconnectNamedPipe(self.pipe) };
        self.accept = Accept::Listening;
    }

    /// Start the overlapped connect on `ov`. Returns `Some` when this slice is over.
    fn start_connect(&mut self) -> Option<HelloWait> {
        // SAFETY: an overlapped connect whose `OVERLAPPED` lives in `self.ov` (boxed) until it
        // completes or is cancelled; the event is reset before it starts.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.event;
            ResetEvent(self.event);
            if ConnectNamedPipe(self.pipe, &mut *self.ov) != 0 { 0 } else { GetLastError() }
        };
        match err {
            ERROR_IO_PENDING => {
                self.accept = Accept::Connecting;
                None
            }
            // Connected already (the bootstrap won the race with this call): read its hello.
            0 | ERROR_PIPE_CONNECTED => self.start_read(),
            e => Some(HelloWait::PipeError(format!("ConnectNamedPipe: {e}"))),
        }
    }

    /// Start the overlapped read of the client's first message into `buf`.
    fn start_read(&mut self) -> Option<HelloWait> {
        // SAFETY: an overlapped read into `buf` (never resized) whose `OVERLAPPED` is
        // `self.ov`; both live in `self` until the read completes or is cancelled.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(
                self.pipe,
                self.buf.as_mut_ptr(),
                self.buf.len() as u32,
                std::ptr::null_mut(),
                &mut *self.ov,
            );
            if ok != 0 { 0 } else { GetLastError() }
        };
        match err {
            0 => {
                // Completed at once: collect the byte count from `ov`.
                let mut n = 0u32;
                // SAFETY: the read on `ov` has completed; no wait.
                unsafe { GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) };
                Some(self.take_hello(n))
            }
            ERROR_IO_PENDING => {
                self.accept = Accept::Reading;
                None
            }
            // The client left (broken pipe) or sent an oversized frame: drop it and accept
            // again; the bootstrap's connect loop retries.
            _ => {
                self.drop_client();
                Some(HelloWait::Timeout)
            }
        }
    }

    /// Classify the `n`-byte first message in `buf`; the pipe then belongs to the relay.
    fn take_hello(&mut self, n: u32) -> HelloWait {
        if n == 0 {
            self.drop_client();
            return HelloWait::Timeout;
        }
        self.accept = Accept::Connected;
        // Portable and unit-tested; maps another header version to `version_mismatch`.
        hello_wait_from_bytes(&self.buf[..n as usize])
    }

    /// Wait for the pending op on `ov` until `deadline`: `Ok(None)` while it is still
    /// pending (it stays pending), `Ok(Some(bytes))` once it completed, `Err` if it failed.
    fn wait_pending(&mut self, deadline: u64) -> Result<Option<u32>, u32> {
        let left = deadline.saturating_sub(now_ms());
        // SAFETY: waits on our event, then collects `ov`'s result without blocking.
        unsafe {
            if WaitForSingleObject(self.event, wait_ms(left)) != WAIT_OBJECT_0 {
                return Ok(None);
            }
            let mut n = 0u32;
            if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                return Ok(Some(n));
            }
            match GetLastError() {
                ERROR_IO_INCOMPLETE => Ok(None),
                e => Err(e),
            }
        }
    }

    /// One `wait_hello` slice: accept the bootstrap and read its first message, keeping any
    /// pending connect or read across slices (see `Accept`).
    pub(crate) fn accept_hello(&mut self, timeout_ms: u64) -> HelloWait {
        let deadline = now_ms().saturating_add(timeout_ms);
        loop {
            match self.accept {
                Accept::Listening => {
                    if let Some(hw) = self.start_connect() {
                        return hw;
                    }
                }
                Accept::Connecting => match self.wait_pending(deadline) {
                    Ok(None) => return HelloWait::Timeout,
                    Ok(Some(_)) => {
                        if let Some(hw) = self.start_read() {
                            return hw;
                        }
                    }
                    // The client connected and left already: accept again.
                    Err(_) => {
                        self.drop_client();
                        return HelloWait::Timeout;
                    }
                },
                Accept::Reading => match self.wait_pending(deadline) {
                    Ok(None) => return HelloWait::Timeout,
                    Ok(Some(n)) => return self.take_hello(n),
                    // The client left before its hello, or sent an oversized frame.
                    Err(_) => {
                        self.drop_client();
                        return HelloWait::Timeout;
                    }
                },
                Accept::Connected => return HelloWait::PipeError("hello already taken".into()),
            }
        }
    }

    /// Read one message (message mode, overlapped) within `timeout_ms`.
    pub(crate) fn read_message(&self, timeout_ms: u64) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; HEADER_LEN + MAX_PAYLOAD];
        // SAFETY: overlapped read into `buf` bounded by the event wait; a timed-out read is
        // cancelled (and the cancel awaited) before `ov` and `buf` go out of scope.
        unsafe {
            ResetEvent(self.event);
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.event;
            let ok = ReadFile(self.pipe, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), &mut ov);
            let mut n = 0u32;
            if ok == 0 {
                if GetLastError() != ERROR_IO_PENDING {
                    return None;
                }
                if WaitForSingleObject(self.event, wait_ms(timeout_ms)) != WAIT_OBJECT_0 {
                    self.cancel(&ov);
                    return None;
                }
                if GetOverlappedResult(self.pipe, &ov, &mut n, 0) == 0 {
                    return None;
                }
            } else {
                GetOverlappedResult(self.pipe, &ov, &mut n, 0);
            }
            (n > 0).then(|| buf[..n as usize].to_vec())
        }
    }

    pub(crate) fn write_message(&self, m: &Message) -> bool {
        let bytes = encode(m);
        // SAFETY: overlapped write; wait for completion, cancelling (and awaiting the cancel)
        // on timeout before `ov` and `bytes` go out of scope.
        unsafe {
            ResetEvent(self.event);
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.event;
            let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
            if WaitForSingleObject(self.event, wait_ms(RELAY_TIMEOUT_MS)) != WAIT_OBJECT_0 {
                self.cancel(&ov);
                return false;
            }
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // A pending accept still writes into `ov` and `buf`: cancel it and wait for the cancel
        // before they are freed (plan-review I5).
        if matches!(self.accept, Accept::Connecting | Accept::Reading) {
            self.cancel(&*self.ov);
        }
        for h in [self.pipe, self.event] {
            // SAFETY: closes handles this server created and still owns.
            unsafe { CloseHandle(h) };
        }
    }
}
```

`crates/uw-helper/src/bin/uw-helper64/relay.rs` (complete file):

```rust
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
```

Run: `git rm -q crates/uw-helper/src/bin/uw-helper64.rs`

- [ ] **Step 3: Both clippies and the unit tests**

Run:
```bash
cargo clippy --target x86_64-pc-windows-gnu -p uw-helper --all-targets -- -D warnings
cargo clippy -p uw-helper --all-targets -- -D warnings
cargo test -p uw-helper
```
Expected: both clippies clean; `decide` 27 passed; Wine tests ignored (7).

- [ ] **Step 4: Plan 1's Wine suites stay green**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: helper `7 passed`, `uw-inject` `1 passed`; no leftovers.

- [ ] **Step 5: Commit**

```bash
git add -A crates/uw-helper
git commit -m "channel: split uw-helper64 by job (no behaviour change)"
```

---

## Task 6: The helper creates the channel before hooking, and exits when stdin closes

Design flow step 2 ("The helper creates (in order) the doorbell events, the mask region, the state block, the overlay pipe, then the bootstrap pipe, and hooks") and "Failures" ("The helper exits when its stdin closes, in every phase"). `decide` gains `Win32::create_channel`, called right before `create_pipe` on both paths; a foreign state block is `version_mismatch` before any pipe or hook; another failure is `channel_failed`. The real helper creates `uw_game_channel::objects::Objects` and the overlay `PipeServer` (held in `win32::Channel`; served from Task 7, hence the temporary `allow(dead_code)`), and a stdin reader thread now runs from the start and exits the process when stdin closes. Commands that arrive while the helper still hooks are not queued (design: "No queueing while disconnected"): right after `decide::run` returns, before `bootstrap_connected` goes out, `Commands::refuse_queued` answers each with `bootstrap_not_connected` (`load_overlay`) or `overlay_not_connected` (`set_state`, `ping`). Task 11's slow-start test checks it.

**Files:**
- Modify: `crates/uw-helper/src/decide.rs`, `crates/uw-helper/tests/decide.rs`, `crates/uw-helper/Cargo.toml`
- Modify: `crates/uw-helper/src/bin/uw-helper64/{main,win32,relay}.rs`
- Create: `crates/uw-helper/src/bin/uw-helper64/commands.rs`

- [ ] **Step 1: Write the failing `decide` tests**

In `crates/uw-helper/tests/decide.rs`, make these 6 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust

use uw_helper::decide::{
    hello_wait_from_bytes, run, Config, HelloWait, HookKind, Outcome, Win32, REWAKE_MS,
};
use uw_pipe_proto::jsonl::Event;
```

with:

```rust

use uw_helper::decide::{
    hello_wait_from_bytes, run, ChannelError, Config, HelloWait, HookKind, Outcome, Win32, REWAKE_MS,
};
use uw_pipe_proto::jsonl::Event;
```

2. Replace:

```rust
    marker: bool,
    lock_free: bool,
    pipe: Result<(), String>,
    getmessage_ok: bool,
```

with:

```rust
    marker: bool,
    lock_free: bool,
    channel: Result<(), ChannelError>,
    pipe: Result<(), String>,
    getmessage_ok: bool,
```

3. Replace:

```rust
            marker: false,
            lock_free: true,
            pipe: Ok(()),
            getmessage_ok: true,
```

with:

```rust
            marker: false,
            lock_free: true,
            channel: Ok(()),
            pipe: Ok(()),
            getmessage_ok: true,
```

4. Replace:

```rust
    fn take_helper_lock(&mut self, _pid: u32) -> bool {
        self.lock_free
    }
    fn create_pipe(&mut self, _pid: u32) -> Result<(), String> {
```

with:

```rust
    fn take_helper_lock(&mut self, _pid: u32) -> bool {
        self.lock_free
    }
    fn create_channel(&mut self, _pid: u32) -> Result<(), ChannelError> {
        self.log.push("create_channel".into());
        self.channel.clone()
    }
    fn create_pipe(&mut self, _pid: u32) -> Result<(), String> {
```

5. Replace:

```rust
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
    assert_eq!(Outcome::PipeError("x".into()).to_event(), Event::error("pipe_error"));
}
```

with:

```rust
    assert_eq!(Outcome::VersionMismatch.to_event(), Event::error("version_mismatch"));
    assert_eq!(Outcome::PipeError("x".into()).to_event(), Event::error("pipe_error"));
    assert_eq!(Outcome::ChannelFailed("x".into()).to_event(), Event::error("channel_failed"));
}
```

6. Replace:

```rust
    assert_eq!(Outcome::GameExited.to_event(), Event::GameExited);
}
```

with:

```rust
    assert_eq!(Outcome::GameExited.to_event(), Event::GameExited);
}

// --- The control channel (plan 2a) ---

fn position(f: &Fake, entry: &str) -> usize {
    f.log.iter().position(|c| c == entry).unwrap_or_else(|| panic!("no {entry} in {:?}", f.log))
}

/// Catches the bootstrap pipe or the hook coming before the channel (design 2a flow step 2: the
/// objects and the overlay pipe exist before the bootstrap can connect).
#[test]
fn the_channel_is_created_before_the_bootstrap_pipe_and_the_hook() {
    let mut f = Fake::happy();
    f.hellos.push_back(hello(PROTOCOL_VERSION, false, None));
    assert_eq!(run(&mut f, &cfg()), Outcome::Connected { overlay_loaded: false, overlay_path: None });
    assert!(position(&f, "create_channel") < position(&f, "create_pipe"));
    assert!(position(&f, "create_pipe") < position(&f, "set_hook GetMessage"));

    // The reconnect path (marker present) creates them in the same order.
    let mut f = Fake::happy();
    f.marker = true;
    f.hellos.push_back(hello(PROTOCOL_VERSION, true, Some(r"Z:\g\overlay.dll")));
    run(&mut f, &cfg());
    assert!(position(&f, "create_channel") < position(&f, "create_pipe"));
}

/// Catches a foreign state block (held by the game from another client version) being used, or
/// reported as anything but `version_mismatch`, or the game being hooked anyway.
#[test]
fn a_foreign_state_block_is_version_mismatch_before_any_pipe_or_hook() {
    for marker in [false, true] {
        let mut f = Fake::happy();
        f.marker = marker;
        f.channel = Err(ChannelError::VersionMismatch);
        assert_eq!(run(&mut f, &cfg()), Outcome::VersionMismatch, "marker {marker}");
        assert!(!f.log.iter().any(|c| c == "create_pipe" || c.starts_with("set_hook")), "{:?}", f.log);
    }
}

#[test]
fn a_channel_that_cannot_be_created_is_channel_failed() {
    let mut f = Fake::happy();
    f.channel = Err(ChannelError::Failed("CreateEventW: 5".into()));
    assert_eq!(run(&mut f, &cfg()), Outcome::ChannelFailed("CreateEventW: 5".into()));
    assert!(!f.log.iter().any(|c| c.starts_with("set_hook")));
}
```


- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-helper --test decide`
Expected: does not compile: unresolved import `uw_helper::decide::ChannelError`; `create_channel` is not a member of trait `Win32`.

- [ ] **Step 3: Add `create_channel` to `decide`**

In `crates/uw-helper/src/decide.rs`, make these 8 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! The helper's portable decision logic: wait for the game and its UI thread, take the helper
//! lock, skip the hook when the marker mutex already exists (the pinned bootstrap will reconnect),
//! otherwise set `WH_GETMESSAGE` (fall back to `WH_CALLWNDPROC` on a set failure or a hello
//! timeout), post wake-ups and re-wake every `REWAKE_MS` while waiting, and classify the `hello`.
//! The game exiting during any wait ends the run with `GameExited`. Side effects go through the
```

with:

```rust
//! The helper's portable decision logic: wait for the game and its UI thread, take the helper
//! lock, create the control channel (doorbells, mask region, state block, overlay pipe) and then
//! the bootstrap pipe, skip the hook when the marker mutex already exists (the pinned bootstrap
//! will reconnect), otherwise set `WH_GETMESSAGE` (fall back to `WH_CALLWNDPROC` on a set failure or a hello
//! timeout), post wake-ups and re-wake every `REWAKE_MS` while waiting, and classify the `hello`.
//! The game exiting during any wait ends the run with `GameExited`. Side effects go through the
```

2. Replace:

```rust
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
```

with:

```rust
}

/// Why `Win32::create_channel` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// A state block the game held across a helper restart is not layout v1 (design 2a:
    /// `version_mismatch`, "restart the game").
    VersionMismatch,
    /// An object or the overlay pipe could not be created.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
```

3. Replace:

```rust
    Connected { overlay_loaded: bool, overlay_path: Option<String> },
    PipeError(String),
}
```

with:

```rust
    Connected { overlay_loaded: bool, overlay_path: Option<String> },
    PipeError(String),
    /// The control channel's objects or the overlay pipe could not be created.
    ChannelFailed(String),
}
```

4. Replace:

```rust
            // here so the host still sees a structured error rather than a silent exit.
            Outcome::PipeError(_) => Event::error("pipe_error"),
        }
    }
```

with:

```rust
            // here so the host still sees a structured error rather than a silent exit.
            Outcome::PipeError(_) => Event::error("pipe_error"),
            // Like `pipe_error`: a helper-local failure, reported so the host sees a structured
            // error rather than a silent exit.
            Outcome::ChannelFailed(_) => Event::error("channel_failed"),
        }
    }
```

5. Replace:

```rust
    /// Take `Local\underwolf-helper-<pid>`; `false` means another helper already holds it.
    fn take_helper_lock(&mut self, pid: u32) -> bool;
    /// Create the pipe server before hooking (design step 4).
    fn create_pipe(&mut self, pid: u32) -> Result<(), String>;
    /// Set a thread-scoped hook; `Err` carries `GetLastError`.
```

with:

```rust
    /// Take `Local\underwolf-helper-<pid>`; `false` means another helper already holds it.
    fn take_helper_lock(&mut self, pid: u32) -> bool;
    /// Create the control channel (design 2a flow step 2: the doorbell events, the mask region,
    /// the state block, then the overlay pipe). Called right before `create_pipe`.
    fn create_channel(&mut self, pid: u32) -> Result<(), ChannelError>;
    /// Create the bootstrap pipe server before hooking (design step 4).
    fn create_pipe(&mut self, pid: u32) -> Result<(), String>;
    /// Set a thread-scoped hook; `Err` carries `GetLastError`.
```

6. Replace:

```rust
        Err(e) => HelloWait::PipeError(format!("decode hello: {e}")),
    }
}
```

with:

```rust
        Err(e) => HelloWait::PipeError(format!("decode hello: {e}")),
    }
}

/// The channel, then the bootstrap pipe (design 2a flow step 2). `Err` ends the run.
fn create_pipes(w: &mut dyn Win32, pid: u32) -> Result<(), Outcome> {
    match w.create_channel(pid) {
        Ok(()) => {}
        Err(ChannelError::VersionMismatch) => return Err(Outcome::VersionMismatch),
        Err(ChannelError::Failed(e)) => return Err(Outcome::ChannelFailed(e)),
    }
    w.create_pipe(pid).map_err(Outcome::PipeError)
}
```

7. Replace:

```rust

    // 3. Already there? The pinned bootstrap from an earlier helper will reconnect: create the
    //    pipe, skip the hook, and wait for its hello (no hook, so nothing to wake).
    if w.marker_exists(pid) {
        w.log("marker present; skipping hook");
        if let Err(e) = w.create_pipe(pid) {
            return Outcome::PipeError(e);
        }
        return await_hello(w, pid, None, cfg.hello_timeout_ms).unwrap_or(Outcome::BootstrapTimeout);
```

with:

```rust

    // 3. Already there? The pinned bootstrap from an earlier helper will reconnect: create the
    //    channel and the pipe, skip the hook, and wait for its hello (no hook, so nothing to wake).
    if w.marker_exists(pid) {
        w.log("marker present; skipping hook");
        if let Err(o) = create_pipes(w, pid) {
            return o;
        }
        return await_hello(w, pid, None, cfg.hello_timeout_ms).unwrap_or(Outcome::BootstrapTimeout);
```

8. Replace:

```rust
        w.sleep_ms(cfg.window_poll_ms);
    };
    if let Err(e) = w.create_pipe(pid) {
        return Outcome::PipeError(e);
    }
```

with:

```rust
        w.sleep_ms(cfg.window_poll_ms);
    };
    if let Err(o) = create_pipes(w, pid) {
        return o;
    }
```


Run: `cargo test -p uw-helper --test decide`
Expected: `30 passed`.

- [ ] **Step 4: Create the channel in the real helper, and read stdin from the start**

In `crates/uw-helper/Cargo.toml`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```toml

# The Wine-side helper (`uw-helper64.exe`): finds the game's UI thread and sets a thread hook that
# makes Wine load `uw_bootstrap64.dll` into the game, then serves the named pipe and relays JSON
# lines. The portable decision logic is in `src/lib.rs` (`decide`); the Win32 wiring is the bin.
[lib]
name = "uw_helper"
```

with:

```toml

# The Wine-side helper (`uw-helper64.exe`): finds the game's UI thread and sets a thread hook that
# makes Wine load `uw_bootstrap64.dll` into the game, creates the control channel's objects
# (`uw-game-channel`), then serves the bootstrap and overlay pipes and relays JSON lines. The portable decision logic is in `src/lib.rs` (`decide`); the Win32 wiring is the bin.
[lib]
name = "uw_helper"
```

2. Replace:

```toml

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }
```

with:

```toml

[dependencies]
uw-game-channel = { path = "../uw-game-channel" }
uw-pipe-proto = { path = "../uw-pipe-proto" }
```


`crates/uw-helper/src/bin/uw-helper64/commands.rs` (complete file):

```rust
//! The stdin reader. One thread reads JSON-lines commands for the whole run, from before the hook
//! on, and hands each to the relay through a channel. When stdin closes the helper exits at once,
//! in every phase (design 2a "Failures": no orphan helper keeps the helper mutex).

use std::io::BufRead;
use std::sync::mpsc::{channel, Receiver};

use uw_pipe_proto::jsonl::{Command, Event};

use crate::relay::emit;

pub(crate) struct Commands {
    pub(crate) rx: Receiver<Command>,
}

impl Commands {
    /// Answer every command that came before the bootstrap connected, at once (design: no
    /// queueing while disconnected): `load_overlay` needs the bootstrap, `set_state` and `ping`
    /// the overlay. `main` calls this before `bootstrap_connected` goes out, so the host's
    /// reconcile commands, sent in answer to it, reach the relay.
    pub(crate) fn refuse_queued(&self) {
        while let Ok(cmd) = self.rx.try_recv() {
            let code = match cmd {
                Command::LoadOverlay { .. } => "bootstrap_not_connected",
                Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
            };
            emit(&Event::reply_error(cmd.request_id(), code));
        }
    }
}

pub(crate) fn spawn_stdin_reader() -> Commands {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match Command::parse(&line) {
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
            }
        }
        eprintln!("[uw-helper64] stdin closed; exiting");
        std::process::exit(0);
    });
    Commands { rx }
}
```

In `crates/uw-helper/src/bin/uw-helper64/win32.rs`, make these 6 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! The real `Win32` for `decide`: Toolhelp and window lookups, the helper and marker mutexes, the
//! thread hook and its wake-ups, and the bootstrap pipe (a `PipeServer`).

use std::panic::{catch_unwind, AssertUnwindSafe};

use uw_helper::decide::{HelloWait, HookKind, Win32};
use uw_pipe_proto::names::{helper_mutex_name, marker_mutex_name, pipe_name};

use windows_sys::Win32::Foundation::{
```

with:

```rust
//! The real `Win32` for `decide`: Toolhelp and window lookups, the helper and marker mutexes, the
//! thread hook and its wake-ups, the control channel (objects and overlay pipe) and the bootstrap
//! pipe.

use std::panic::{catch_unwind, AssertUnwindSafe};

use uw_game_channel::objects::{Objects, ObjectsError};
use uw_helper::decide::{ChannelError, HelloWait, HookKind, Win32};
use uw_pipe_proto::names::{helper_mutex_name, marker_mutex_name, overlay_pipe_name, pipe_name};

use windows_sys::Win32::Foundation::{
```

2. Replace:

```rust
}

pub(crate) struct RealWin32 {
    exe: String,
```

with:

```rust
}

/// The control channel: the objects the game opens after its `overlay_hello`, and the overlay
/// pipe it connects to. Held here so both exist from before the hook on; the relay loop (plan 2a
/// Task 7) serves them and removes this `allow`.
#[allow(dead_code)]
pub(crate) struct Channel {
    pub(crate) objects: Objects,
    pub(crate) overlay: PipeServer,
}

pub(crate) struct RealWin32 {
    exe: String,
```

3. Replace:

```rust
    module: HMODULE,
    helper_mutex: HANDLE,
    /// The bootstrap pipe, from `create_pipe` on.
    pub(crate) pipe: Option<PipeServer>,
```

with:

```rust
    module: HMODULE,
    helper_mutex: HANDLE,
    /// The control channel, from `create_channel` on.
    pub(crate) channel: Option<Channel>,
    /// The bootstrap pipe, from `create_pipe` on.
    pub(crate) pipe: Option<PipeServer>,
```

4. Replace:

```rust
            module: std::ptr::null_mut(),
            helper_mutex: std::ptr::null_mut(),
            pipe: None,
        }
```

with:

```rust
            module: std::ptr::null_mut(),
            helper_mutex: std::ptr::null_mut(),
            channel: None,
            pipe: None,
        }
```

5. Replace:

```rust
            unsafe { CloseHandle(self.helper_mutex) };
        }
        // `pipe` drops after this: it cancels any pending accept and closes its handles.
    }
}
```

with:

```rust
            unsafe { CloseHandle(self.helper_mutex) };
        }
        // `channel` and `pipe` drop after this: each pipe cancels any pending accept and closes
        // its handles.
    }
}
```

6. Replace:

```rust
        unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
    }
    fn create_pipe(&mut self, pid: u32) -> Result<(), String> {
        self.pipe = Some(PipeServer::create(&pipe_name(pid))?);
```

with:

```rust
        unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
    }
    fn create_channel(&mut self, pid: u32) -> Result<(), ChannelError> {
        let objects = Objects::create(pid).map_err(|e| match e {
            ObjectsError::VersionMismatch(h) => {
                eprintln!("[uw-helper64] state block: {h}; restart the game");
                ChannelError::VersionMismatch
            }
            other => ChannelError::Failed(other.to_string()),
        })?;
        let overlay = PipeServer::create(&overlay_pipe_name(pid)).map_err(ChannelError::Failed)?;
        self.channel = Some(Channel { objects, overlay });
        Ok(())
    }
    fn create_pipe(&mut self, pid: u32) -> Result<(), String> {
        self.pipe = Some(PipeServer::create(&pipe_name(pid))?);
```


In `crates/uw-helper/src/bin/uw-helper64/relay.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! The relay: JSON-lines commands from stdin to the bootstrap pipe and replies back as events on
//! stdout, plus the game watcher that reports `game_exited`.

use std::io::{BufRead, Write};

use uw_pipe_proto::jsonl::{Command, Event};
```

with:

```rust
//! The relay: JSON-lines commands (from the stdin reader) to the bootstrap pipe and replies back
//! as events on stdout, plus the game watcher that reports `game_exited`.

use std::io::Write;

use uw_pipe_proto::jsonl::{Command, Event};
```

2. Replace:

```rust
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};

use crate::pipe_server::{PipeServer, RELAY_TIMEOUT_MS};
```

with:

```rust
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};

use crate::commands::Commands;
use crate::pipe_server::{PipeServer, RELAY_TIMEOUT_MS};
```

3. Replace:

```rust

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
```

with:

```rust

/// Relay host commands to the bootstrap and status replies back. A watcher thread reports
/// `game_exited` the moment the game process ends, even while this waits for a command.
pub(crate) fn relay(pipe: &mut PipeServer, pid: u32, commands: &Commands) {
    spawn_game_watcher(pid);
    for cmd in commands.rx.iter() {
        match cmd {
            Command::LoadOverlay { request_id, path } => {
                if path.encode_utf16().count() > MAX_PATH_UNITS {
                    emit(&Event::reply_error(request_id, "path_too_long"));
```

4. Replace:

```rust
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
```

with:

```rust
                }
            }
            // The overlay pipe is not served yet: commands that need the overlay fail (design:
            // no queueing while disconnected).
            cmd @ (Command::SetState { .. } | Command::Ping { .. }) => {
                emit(&Event::reply_error(cmd.request_id(), "overlay_not_connected"));
            }
        }
    }
```


In `crates/uw-helper/src/bin/uw-helper64/main.rs`, make these 5 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//!
//! Modules: `win32` (the real `Win32` for `decide`), `pipe_server` (the overlapped pipe server),
//! `relay` (the JSON-lines relay and the game watcher).

#[cfg(not(windows))]
```

with:

```rust
//!
//! Modules: `win32` (the real `Win32` for `decide`), `pipe_server` (the overlapped pipe server),
//! `commands` (the stdin reader; stdin closing ends the helper in every phase), `relay` (the
//! JSON-lines relay and the game watcher).

#[cfg(not(windows))]
```

2. Replace:

```rust
}

#[cfg(windows)]
mod pipe_server;
```

with:

```rust
}

#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod pipe_server;
```

3. Replace:

```rust
    }

    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
```

with:

```rust
    }

    // Read stdin from the start, so its closing ends the helper in every phase.
    let commands = commands::spawn_stdin_reader();
    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
```

4. Replace:

```rust
    };
    let outcome = run(&mut w, &cfg);
    relay::emit(&outcome.to_event());
    match outcome {
```

with:

```rust
    };
    let outcome = run(&mut w, &cfg);
    commands.refuse_queued();
    relay::emit(&outcome.to_event());
    match outcome {
```

5. Replace:

```rust
            let pid = w.pid;
            if let Some(pipe) = w.pipe.as_mut() {
                relay::relay(pipe, pid);
            }
            0
```

with:

```rust
            let pid = w.pid;
            if let Some(pipe) = w.pipe.as_mut() {
                relay::relay(pipe, pid, &commands);
            }
            0
```


- [ ] **Step 5: Both clippies**

Run:
```bash
cargo clippy --target x86_64-pc-windows-gnu -p uw-helper --all-targets -- -D warnings
cargo clippy -p uw-helper --all-targets -- -D warnings
```
Expected: both clean.

- [ ] **Step 6: Plan 1's helper Wine suite stays green**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: `7 passed`; no leftovers. (Stdin closing in each phase is tested end to end in Task 11.)

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates/uw-helper
git commit -m "channel: the helper creates the channel before hooking and exits when stdin closes"
```

---

## Task 7: The helper's relay loop over commands, `bell-host` and both pipes

Design "Components and flow" (steps 3-6), "Threads" ("waits on its pipes and `bell-host` in one loop"), "Failures" and "Plan 1 items". After the bootstrap's first `hello`, one `WaitForMultipleObjects` loop serves:
- **commands** (the stdin reader now signals an event): `load_overlay` to the bootstrap pipe, tracked in `Pending` until its `status` (a `status` whose id is not pending is discarded: plan 1 final review M2); `set_state` under the state lock (100 ms, else `state_busy`) with `state::apply`, then `ring_game` with the changed bits and `state_set`; `ping` to the overlay pipe, answered by its `pong`, or after the ping timeout (15 s, `UW_PING_TIMEOUT_MS` in tests) by `overlay_not_responding`, its late `pong` discarded (owner decision D1; pings are a second `Pending`);
- **`bell-host`**: swap `to_host`, read `applied_seq` under the lock, emit `bell`;
- **the bootstrap pipe**: replies, its drop (`bootstrap_disconnected`; pending loads fail with `bootstrap_not_connected`) and its reconnect `hello` (`bootstrap_connected` again);
- **the overlay pipe**: the first message must be a supported `overlay_hello` (`overlay_connected`, else `version_mismatch` or `pipe_error` and the connection is dropped), then `pong`s; an unknown type or bad message, an oversized one included (`PipeEvent::Oversized`, from `ERROR_MORE_DATA`), is `pipe_error` and the helper listens again; its drop is `overlay_disconnected`.

`PipeServer` is rewritten around a non-blocking `poll` (one connect or read always pending on its event; writes on their own event), which also serves `decide`'s `wait_hello` slices.

**Files:**
- Create: `crates/uw-helper/src/pending.rs`, `crates/uw-helper/tests/pending.rs`
- Modify: `crates/uw-helper/src/lib.rs`
- Replace: `crates/uw-helper/src/bin/uw-helper64/pipe_server.rs`, `…/relay.rs`, `…/commands.rs`
- Modify: `crates/uw-helper/src/bin/uw-helper64/{main,win32}.rs`, `crates/uw-helper/tests/wine_e2e.rs`

- [ ] **Step 1: Write the failing `Pending` tests**

`crates/uw-helper/tests/pending.rs` (complete file):

```rust
use uw_helper::pending::Pending;

/// Catches plan 1's final-review M2: after a `status_timeout`, the late `status` must not be taken
/// as the reply to the next request.
#[test]
fn a_late_status_is_discarded_and_the_next_request_gets_its_own() {
    let mut p = Pending::new();
    p.add(1, 15_000);
    assert_eq!(p.expire(14_999), Vec::<u32>::new());
    assert_eq!(p.expire(15_000), vec![1]);
    // Request 1's status arrives late: not pending any more, so discarded.
    assert!(!p.take(1));
    p.add(2, 40_000);
    assert!(!p.take(1), "a second late copy is discarded too");
    assert!(p.take(2));
    assert_eq!(p.next_deadline(), None);
}

/// Catches one request's reply or timeout touching another.
#[test]
fn requests_time_out_and_resolve_independently() {
    let mut p = Pending::new();
    p.add(1, 100);
    p.add(2, 50);
    assert_eq!(p.next_deadline(), Some(50));
    assert_eq!(p.expire(60), vec![2]);
    assert_eq!(p.next_deadline(), Some(100));
    assert!(p.take(1));
    assert!(!p.take(1), "answered once");
    p.add(3, 10);
    p.add(4, 20);
    assert_eq!(p.drain(), vec![3, 4]);
    assert_eq!(p.next_deadline(), None);
}

/// Catches owner decision D1 breaking at the bookkeeping level: a ping past its deadline expires
/// once (one `overlay_not_responding`), and its late `pong` is then discarded, while a ping still
/// in time is answered.
#[test]
fn a_ping_past_its_deadline_expires_once_and_its_late_pong_is_discarded() {
    let mut pings = Pending::new();
    pings.add(10, 15_000);
    pings.add(11, 30_000);
    assert_eq!(pings.expire(15_000), vec![10]);
    assert_eq!(pings.expire(15_001), Vec::<u32>::new(), "expired once");
    assert!(!pings.take(10), "the late pong is discarded");
    assert!(pings.take(11));
}
```

Run: `cargo test -p uw-helper --test pending`
Expected: does not compile: unresolved import `uw_helper::pending`.

- [ ] **Step 2: Add `Pending`**

`crates/uw-helper/src/pending.rs` (complete file):

```rust
//! Requests the relay sent and is waiting on: `load_overlay`s awaiting their `status` (design 2a
//! "Plan 1 items", late reply) and `ping`s awaiting their `pong` (owner decision D1). Each has its
//! own deadline; a reply whose id is not pending (a late reply to a request that already timed
//! out, or a stray one) is discarded, so every command gets exactly one reply.

#[derive(Debug, Default)]
pub struct Pending {
    entries: Vec<(u32, u64)>,
}

impl Pending {
    pub fn new() -> Pending {
        Pending::default()
    }

    /// `id` was sent; it times out at `deadline_ms`.
    pub fn add(&mut self, id: u32, deadline_ms: u64) {
        self.entries.push((id, deadline_ms));
    }

    /// A `status` for `id` arrived: `true` (and forgotten) if it was pending, `false` to discard it.
    pub fn take(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(p, _)| *p != id);
        self.entries.len() != before
    }

    /// The requests past their deadline at `now_ms`, forgotten (each gets `status_timeout`).
    pub fn expire(&mut self, now_ms: u64) -> Vec<u32> {
        let (late, keep): (Vec<_>, Vec<_>) = self.entries.iter().partition(|(_, d)| now_ms >= *d);
        self.entries = keep;
        late.into_iter().map(|(id, _)| id).collect()
    }

    /// Every pending request, forgotten (the bootstrap went away).
    pub fn drain(&mut self) -> Vec<u32> {
        self.entries.drain(..).map(|(id, _)| id).collect()
    }

    /// The earliest deadline, for the relay's wait.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|(_, d)| *d).min()
    }
}
```

`crates/uw-helper/src/lib.rs` (complete file):

```rust
//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine, and `pending` the relay's `load_overlay` requests
//! awaiting their `status` (both unit-tested on Linux). The real `Win32` implementation, the pipe
//! servers and the relay loop live in the `cfg(windows)` bin `uw-helper64`.

pub mod decide;
pub mod pending;
```

Run: `cargo test -p uw-helper --test pending --test decide`
Expected: `pending` 3, `decide` 30, all pass.

- [ ] **Step 3: Replace the pipe server, the stdin reader and the relay**

`crates/uw-helper/src/bin/uw-helper64/pipe_server.rs` (complete file):

```rust
//! The helper's pipe server, used for both the bootstrap pipe and the overlay pipe: one
//! overlapped, message-mode, local-only duplex pipe instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`,
//! one instance). `poll` advances its accept-and-read state machine without blocking: there is
//! always one connect or one read pending on `ov`, signalled through `event`, so the relay can
//! wait on every pipe at once and `decide`'s `wait_hello` can wait in slices without ever
//! dropping a client. Writes use their own event and `OVERLAPPED`, so they never disturb the
//! pending read.

use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_pipe_proto::pipe::{HEADER_LEN, MAX_PAYLOAD};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_CONNECTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::{wait_ms, wide};

/// How long one pipe write may take, and the default `status` and `ping` timeouts.
pub(crate) const RELAY_TIMEOUT_MS: u64 = 15_000;

pub(crate) fn now_ms() -> u64 {
    // SAFETY: a monotonic tick count.
    unsafe { GetTickCount64() }
}

/// What `poll` saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PipeEvent {
    /// A client connected (its first message comes next).
    Connected,
    /// One whole message from the client.
    Message(Vec<u8>),
    /// The client left; the server listens again.
    Disconnected,
    /// The client sent a message over the frame limit (header + `MAX_PAYLOAD`): a bad message
    /// (design: `pipe_error`). The client is dropped and the server listens again.
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing pending: the next `poll` starts an overlapped `ConnectNamedPipe`.
    Listening,
    /// A `ConnectNamedPipe` is pending on `ov`.
    Connecting,
    /// A client is connected and a `ReadFile` into `buf` is pending on `ov`.
    Reading,
    /// A read failed to start: the client is gone; the next `poll` reports `Disconnected`.
    Broken,
    /// A read completed at once with more data than `buf` holds; the next `poll` reports
    /// `Oversized`.
    Overflowed,
}

/// How the operation on `ov` stands.
enum Io {
    Pending,
    Done(u32),
    /// A read whose message is larger than `buf` (`ERROR_MORE_DATA`).
    TooLarge,
    Failed,
}

pub(crate) struct PipeServer {
    pipe: HANDLE,
    /// Manual-reset event of the connect or read pending on `ov`.
    event: HANDLE,
    /// Boxed so its address stays fixed while the I/O is pending.
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    state: State,
    /// Manual-reset event for writes.
    write_event: HANDLE,
}

fn manual_event() -> HANDLE {
    // SAFETY: an unnamed, manual-reset, initially unsignaled event for overlapped I/O.
    unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
}

impl PipeServer {
    /// Create the pipe instance and its events. `name` is a `\\.\pipe\…` name.
    pub(crate) fn create(name: &str) -> Result<PipeServer, String> {
        let name = wide(name);
        // SAFETY: one overlapped, first-instance, message-mode, local-only duplex pipe.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (HEADER_LEN + MAX_PAYLOAD) as u32,
                (HEADER_LEN + MAX_PAYLOAD) as u32,
                0,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            // SAFETY: no arguments.
            return Err(format!("CreateNamedPipeW: {}", unsafe { GetLastError() }));
        }
        let (event, write_event) = (manual_event(), manual_event());
        if event.is_null() || write_event.is_null() {
            for h in [pipe, event, write_event] {
                if !h.is_null() {
                    // SAFETY: closes a handle just created here.
                    unsafe { CloseHandle(h) };
                }
            }
            return Err("CreateEventW failed".into());
        }
        Ok(PipeServer {
            pipe,
            event,
            // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            state: State::Listening,
            write_event,
        })
    }

    /// Signalled when `poll` may have something: wait on it, then call `poll`.
    pub(crate) fn event(&self) -> HANDLE {
        self.event
    }

    /// Advance without blocking: what happened, or `None` while the pending I/O is still pending.
    pub(crate) fn poll(&mut self) -> Option<PipeEvent> {
        loop {
            match self.state {
                State::Listening => {
                    // SAFETY: an overlapped connect on `ov` (boxed, owned by `self`), its event
                    // reset first.
                    let err = unsafe {
                        *self.ov = std::mem::zeroed();
                        self.ov.hEvent = self.event;
                        ResetEvent(self.event);
                        if ConnectNamedPipe(self.pipe, &mut *self.ov) != 0 { 0 } else { GetLastError() }
                    };
                    match err {
                        ERROR_IO_PENDING => {
                            self.state = State::Connecting;
                            return None;
                        }
                        // Connected already (the client won the race with this call).
                        0 | ERROR_PIPE_CONNECTED => {
                            self.start_read();
                            return Some(PipeEvent::Connected);
                        }
                        // A client that came and went (`ERROR_NO_DATA`) or another failure: free
                        // the instance and try again on the next wake-up (`event` is reset, so
                        // this does not spin).
                        e => {
                            eprintln!("[uw-helper64] ConnectNamedPipe: {e}");
                            // SAFETY: disconnects our instance; no I/O is pending on it.
                            unsafe { DisconnectNamedPipe(self.pipe) };
                            return None;
                        }
                    }
                }
                State::Connecting => match self.result() {
                    Io::Pending => return None,
                    Io::Done(_) => {
                        self.start_read();
                        return Some(PipeEvent::Connected);
                    }
                    // The client connected and left already: accept again.
                    Io::TooLarge | Io::Failed => self.reset(),
                },
                State::Reading => match self.result() {
                    Io::Pending => return None,
                    Io::Done(n) if n > 0 => {
                        let msg = self.buf[..n as usize].to_vec();
                        self.start_read();
                        return Some(PipeEvent::Message(msg));
                    }
                    Io::TooLarge => {
                        self.reset();
                        return Some(PipeEvent::Oversized);
                    }
                    // A zero-length message or a closed client.
                    Io::Done(_) | Io::Failed => {
                        self.reset();
                        return Some(PipeEvent::Disconnected);
                    }
                },
                State::Broken => {
                    self.reset();
                    return Some(PipeEvent::Disconnected);
                }
                State::Overflowed => {
                    self.reset();
                    return Some(PipeEvent::Oversized);
                }
            }
        }
    }

    /// `poll`, waiting up to `timeout_ms` for something to happen.
    pub(crate) fn wait(&mut self, timeout_ms: u64) -> Option<PipeEvent> {
        let deadline = now_ms().saturating_add(timeout_ms);
        loop {
            if let Some(e) = self.poll() {
                return Some(e);
            }
            let left = deadline.saturating_sub(now_ms());
            // SAFETY: waits on our event.
            if left == 0 || unsafe { WaitForSingleObject(self.event, wait_ms(left)) } != WAIT_OBJECT_0 {
                return None;
            }
        }
    }

    /// One `decide::wait_hello` slice: accept a client and read its first message within
    /// `timeout_ms`. A timeout leaves the connect or read pending for the next slice; a client
    /// that leaves before its first message is dropped and the next one accepted.
    pub(crate) fn accept_hello(&mut self, timeout_ms: u64) -> HelloWait {
        let deadline = now_ms().saturating_add(timeout_ms);
        loop {
            match self.wait(deadline.saturating_sub(now_ms())) {
                Some(PipeEvent::Message(bytes)) => return hello_wait_from_bytes(&bytes),
                // A client that left, or sent an oversized first message, is dropped: accept again.
                Some(PipeEvent::Connected | PipeEvent::Disconnected | PipeEvent::Oversized) => continue,
                None => return HelloWait::Timeout,
            }
        }
    }

    /// Drop the client (after a bad message): cancel the pending read and listen again.
    pub(crate) fn disconnect(&mut self) {
        if matches!(self.state, State::Connecting | State::Reading) {
            self.cancel();
        }
        self.reset();
    }

    /// Write one message, waiting at most `RELAY_TIMEOUT_MS`. `false`: the client is gone.
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
        // SAFETY: an overlapped write with its own event; on a timeout it is cancelled and the
        // cancel awaited before `ov` and `bytes` go out of scope.
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.write_event;
            ResetEvent(self.write_event);
            let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
            if WaitForSingleObject(self.write_event, wait_ms(RELAY_TIMEOUT_MS)) != WAIT_OBJECT_0 {
                CancelIoEx(self.pipe, &ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, &ov, &mut n, 1);
                return false;
            }
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
        }
    }

    /// Start the read of the client's next message into `buf`. Its completion, now or later, is
    /// collected by the next `poll`.
    fn start_read(&mut self) {
        // SAFETY: an overlapped read into `buf` (never resized) on `ov`; both live in `self`
        // until the read completes or is cancelled.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.event;
            ResetEvent(self.event);
            let ok = ReadFile(self.pipe, self.buf.as_mut_ptr(), self.buf.len() as u32, std::ptr::null_mut(), &mut *self.ov);
            if ok != 0 { 0 } else { GetLastError() }
        };
        self.state = match err {
            0 | ERROR_IO_PENDING => State::Reading,
            ERROR_MORE_DATA => State::Overflowed,
            _ => State::Broken,
        };
    }

    fn result(&self) -> Io {
        let mut n = 0u32;
        // SAFETY: collects `ov`'s result without waiting.
        unsafe {
            if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                return Io::Done(n);
            }
            match GetLastError() {
                ERROR_IO_INCOMPLETE => Io::Pending,
                ERROR_MORE_DATA => Io::TooLarge,
                _ => Io::Failed,
            }
        }
    }

    /// Cancel the I/O pending on `ov` and wait for the cancel, so `ov` and `buf` are free again.
    fn cancel(&self) {
        // SAFETY: cancels I/O on our pipe and blocks until the cancel completes.
        unsafe {
            CancelIoEx(self.pipe, &*self.ov);
            let mut n = 0u32;
            GetOverlappedResult(self.pipe, &*self.ov, &mut n, 1);
        }
    }

    /// Free the instance for the next client; nothing may be pending on `ov`.
    fn reset(&mut self) {
        // SAFETY: disconnects our instance and resets our event.
        unsafe {
            DisconnectNamedPipe(self.pipe);
            ResetEvent(self.event);
        }
        self.state = State::Listening;
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // A pending connect or read still writes into `ov` and `buf`: cancel it and wait for the
        // cancel before they are freed (plan 1 plan-review I5).
        if matches!(self.state, State::Connecting | State::Reading) {
            self.cancel();
        }
        for h in [self.pipe, self.event, self.write_event] {
            // SAFETY: closes handles this server created and still owns.
            unsafe { CloseHandle(h) };
        }
    }
}
```

`crates/uw-helper/src/bin/uw-helper64/commands.rs` (complete file):

```rust
//! The stdin reader. One thread reads JSON-lines commands for the whole run, from before the hook
//! on, and hands each to the relay through a channel, signalling `event` (an unnamed auto-reset
//! event) so the relay's wait wakes. When stdin closes the helper exits at once, in every phase
//! (design 2a "Failures": no orphan helper keeps the helper mutex).

use std::io::BufRead;
use std::sync::mpsc::{channel, Receiver};

use uw_pipe_proto::jsonl::{Command, Event};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

use crate::relay::emit;

pub(crate) struct Commands {
    pub(crate) rx: Receiver<Command>,
    /// Signalled after each command is queued; never closed (it lives as long as the process).
    pub(crate) event: HANDLE,
}

impl Commands {
    /// Answer every command that came before the bootstrap connected, at once (design: no
    /// queueing while disconnected): `load_overlay` needs the bootstrap, `set_state` and `ping`
    /// the overlay. `main` calls this before `bootstrap_connected` goes out, so the host's
    /// reconcile commands, sent in answer to it, reach the relay.
    pub(crate) fn refuse_queued(&self) {
        while let Ok(cmd) = self.rx.try_recv() {
            let code = match cmd {
                Command::LoadOverlay { .. } => "bootstrap_not_connected",
                Command::SetState { .. } | Command::Ping { .. } => "overlay_not_connected",
            };
            emit(&Event::reply_error(cmd.request_id(), code));
        }
    }
}

pub(crate) fn spawn_stdin_reader() -> Result<Commands, String> {
    // SAFETY: an unnamed auto-reset event, initially unsignaled.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
    if event.is_null() {
        return Err("CreateEventW failed".into());
    }
    let (tx, rx) = channel();
    let signal = event as usize; // a HANDLE is not `Send`; the event is never closed
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match Command::parse(&line) {
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                    // SAFETY: signals the event created above.
                    unsafe { SetEvent(signal as HANDLE) };
                }
                Err(e) => eprintln!("[uw-helper64] bad command: {e}"),
            }
        }
        eprintln!("[uw-helper64] stdin closed; exiting");
        std::process::exit(0);
    });
    Ok(Commands { rx, event })
}
```

`crates/uw-helper/src/bin/uw-helper64/relay.rs` (complete file):

```rust
//! The relay (design 2a "Components and flow", "Threads"): after the bootstrap's first `hello`,
//! one loop waits on everything at once: queued stdin commands, `bell-host`, the bootstrap pipe
//! and the overlay pipe. It relays commands to the pipes and the state block, and replies,
//! connections and bells back as JSON-lines events. A watcher thread reports `game_exited`.
//!
//! Every command gets one reply carrying its `request_id`. No queueing while disconnected:
//! `load_overlay` needs the bootstrap (`bootstrap_not_connected`), `set_state` and `ping` need the
//! overlay (`overlay_not_connected`). A `load_overlay` without its `status` in time is
//! `status_timeout`, a `ping` without its `pong` in time `overlay_not_responding`; a reply that
//! comes after that is discarded.

use std::io::Write;

use uw_game_channel::objects::{LockError, Objects, LOCK_TIMEOUT_MS};
use uw_helper::decide::{hello_wait_from_bytes, HelloWait};
use uw_helper::pending::Pending;
use uw_pipe_proto::bells::{bit_names, to_host};
use uw_pipe_proto::jsonl::{Command, Event};
use uw_pipe_proto::pipe::{
    check_overlay_first, decode, decode_overlay, encode, encode_overlay, Message, OverlayFirst, OverlayMessage,
    MAX_PATH_UNITS, PROTOCOL_VERSION,
};
use uw_pipe_proto::state;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForMultipleObjects, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
};

use crate::commands::Commands;
use crate::pipe_server::{now_ms, PipeEvent, PipeServer};
use crate::wait_ms;
use crate::win32::Channel;

/// The loop wakes at least this often, so a pipe whose connect could not start retries.
const MAX_WAIT_MS: u64 = 1_000;

/// Write one event line to stdout (whole lines, so threads never interleave).
pub(crate) fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_line());
    let _ = out.flush();
}

struct Relay<'a> {
    objects: &'a Objects,
    bootstrap: &'a mut PipeServer,
    overlay: &'a mut PipeServer,
    status_timeout_ms: u64,
    ping_timeout_ms: u64,
    /// The bootstrap sent its `hello` on the current connection.
    bootstrap_up: bool,
    /// A client connected to the bootstrap pipe and its `hello` is next.
    bootstrap_hello_due: bool,
    overlay_up: bool,
    overlay_hello_due: bool,
    loads: Pending,
    /// Pings sent to the overlay, by request id (the ping's nonce), with their deadlines.
    pings: Pending,
}

/// Run the relay until the process exits (stdin closes, or the game exits). On entry the bootstrap
/// is connected and its `hello` has been taken and reported.
pub(crate) fn run(
    pid: u32,
    commands: &Commands,
    channel: &mut Channel,
    bootstrap: &mut PipeServer,
    status_timeout_ms: u64,
    ping_timeout_ms: u64,
) {
    spawn_game_watcher(pid);
    let mut r = Relay {
        objects: &channel.objects,
        bootstrap,
        overlay: &mut channel.overlay,
        status_timeout_ms,
        ping_timeout_ms,
        bootstrap_up: true,
        bootstrap_hello_due: false,
        overlay_up: false,
        overlay_hello_due: false,
        loads: Pending::new(),
        pings: Pending::new(),
    };
    loop {
        r.serve_bootstrap();
        r.serve_overlay();
        while let Ok(cmd) = commands.rx.try_recv() {
            r.command(cmd);
        }
        r.take_bells();
        for id in r.loads.expire(now_ms()) {
            emit(&Event::reply_error(id, "status_timeout"));
        }
        // Owner decision D1 (design "Failures"): a ping the overlay does not answer in time.
        for id in r.pings.expire(now_ms()) {
            emit(&Event::reply_error(id, "overlay_not_responding"));
        }
        let handles = [commands.event, r.objects.bell_host(), r.bootstrap.event(), r.overlay.event()];
        let next = [r.loads.next_deadline(), r.pings.next_deadline()].into_iter().flatten().min();
        let timeout = next.map_or(MAX_WAIT_MS, |d| d.saturating_sub(now_ms()).min(MAX_WAIT_MS));
        // SAFETY: waits on four handles this process owns; the result only says "something may be
        // ready", and every source is polled at the top of the loop.
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, wait_ms(timeout)) };
    }
}

impl Relay<'_> {
    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::LoadOverlay { request_id, path } => {
                if path.encode_utf16().count() > MAX_PATH_UNITS {
                    emit(&Event::reply_error(request_id, "path_too_long"));
                } else if !self.bootstrap_up {
                    emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                } else if self.bootstrap.write(&encode(&Message::LoadOverlay { request_id, path })) {
                    // The id travels to the bootstrap, which echoes it in `status`.
                    self.loads.add(request_id, now_ms().saturating_add(self.status_timeout_ms));
                } else {
                    // A failed write is `bootstrap_disconnected`, not `game_exited` (design 2a).
                    emit(&Event::reply_error(request_id, "bootstrap_not_connected"));
                    self.drop_bootstrap();
                }
            }
            Command::SetState { request_id, edit_mode, visible } => {
                if !self.overlay_up {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                    return;
                }
                let change = match self.objects.lock(LOCK_TIMEOUT_MS) {
                    Ok(mut g) => state::apply(g.bytes_mut(), edit_mode, visible),
                    Err(LockError::Busy) => {
                        emit(&Event::reply_error(request_id, "state_busy"));
                        return;
                    }
                    Err(LockError::BadHeader(h)) => {
                        eprintln!("[uw-helper64] state block: {h}");
                        emit(&Event::reply_error(request_id, "version_mismatch"));
                        return;
                    }
                };
                // Nothing changed: the current seq, no bump, no bell (design 2a).
                if change.bits != 0 {
                    self.objects.ring_game(change.bits);
                }
                emit(&Event::StateSet { request_id, seq: change.seq });
            }
            Command::Ping { request_id } => {
                if !self.overlay_up {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                } else if self.overlay.write(&encode_overlay(&OverlayMessage::Ping { nonce: request_id })) {
                    self.pings.add(request_id, now_ms().saturating_add(self.ping_timeout_ms));
                } else {
                    emit(&Event::reply_error(request_id, "overlay_not_connected"));
                    self.drop_overlay();
                }
            }
        }
    }

    /// Swap `to_host` and report the bits with `applied_seq`. If the lock is busy the bits go
    /// back and the bell is rung again, so the next pass retries (a bell is not a command, so it
    /// is never dropped as `state_busy`).
    fn take_bells(&mut self) {
        let bits = self.objects.take_to_host();
        if bits == 0 {
            return;
        }
        match self.objects.lock(LOCK_TIMEOUT_MS) {
            Ok(g) => {
                let applied_seq = state::read(g.bytes()).applied_seq;
                drop(g);
                emit(&Event::Bell { bits: bit_names(bits, &to_host::NAMES), applied_seq });
            }
            Err(e) => {
                eprintln!("[uw-helper64] bell: state lock {e:?}; retrying");
                self.objects.ring_host(bits);
            }
        }
    }

    fn serve_bootstrap(&mut self) {
        while let Some(ev) = self.bootstrap.poll() {
            match ev {
                PipeEvent::Connected => self.bootstrap_hello_due = true,
                // A bootstrap that reconnected (design: it retries every second).
                PipeEvent::Message(bytes) if self.bootstrap_hello_due => {
                    self.bootstrap_hello_due = false;
                    match hello_wait_from_bytes(&bytes) {
                        HelloWait::Hello { protocol_version, overlay_loaded, overlay_path }
                            if protocol_version == PROTOCOL_VERSION =>
                        {
                            self.bootstrap_up = true;
                            emit(&Event::BootstrapConnected { overlay_loaded, overlay_path });
                        }
                        HelloWait::Hello { .. } => {
                            emit(&Event::error("version_mismatch"));
                            self.bootstrap.disconnect();
                        }
                        _ => {
                            emit(&Event::error("pipe_error"));
                            self.bootstrap.disconnect();
                        }
                    }
                }
                PipeEvent::Message(bytes) => match decode(&bytes) {
                    Ok(Message::Status { request_id, code, detail, message }) => {
                        if self.loads.take(request_id) {
                            emit(&Event::Status { request_id, code: code.as_str().into(), detail, message });
                        } else {
                            // A late reply to a request that already timed out (design 2a).
                            eprintln!("[uw-helper64] discarding a status for request {request_id}: not pending");
                        }
                    }
                    _ => {
                        emit(&Event::error("pipe_error"));
                        self.drop_bootstrap();
                    }
                },
                PipeEvent::Oversized => {
                    emit(&Event::error("pipe_error"));
                    self.bootstrap_lost();
                }
                PipeEvent::Disconnected => self.bootstrap_lost(),
            }
        }
    }

    fn serve_overlay(&mut self) {
        while let Some(ev) = self.overlay.poll() {
            match ev {
                PipeEvent::Connected => self.overlay_hello_due = true,
                PipeEvent::Message(bytes) if self.overlay_hello_due => {
                    self.overlay_hello_due = false;
                    match check_overlay_first(&bytes) {
                        OverlayFirst::Hello { module_version } => {
                            self.overlay_up = true;
                            emit(&Event::OverlayConnected { module_version });
                        }
                        OverlayFirst::VersionMismatch => {
                            emit(&Event::error("version_mismatch"));
                            self.overlay.disconnect();
                        }
                        OverlayFirst::Bad(why) => {
                            eprintln!("[uw-helper64] overlay pipe: {why}");
                            emit(&Event::error("pipe_error"));
                            self.overlay.disconnect();
                        }
                    }
                }
                PipeEvent::Message(bytes) => match decode_overlay(&bytes) {
                    Ok(OverlayMessage::Pong { nonce, seen_seq }) => {
                        if self.pings.take(nonce) {
                            emit(&Event::Pong { request_id: nonce, seen_seq });
                        } else {
                            // A late pong to a ping that already got `overlay_not_responding`.
                            eprintln!("[uw-helper64] discarding a pong for {nonce}: not pending");
                        }
                    }
                    // An unknown type or a bad message: `pipe_error`, drop it, listen again.
                    _ => {
                        emit(&Event::error("pipe_error"));
                        self.drop_overlay();
                    }
                },
                // An oversized message is a bad message too (design "Failures"): `pipe_error`;
                // the pipe server already dropped it and listens again.
                PipeEvent::Oversized => {
                    emit(&Event::error("pipe_error"));
                    self.overlay_lost();
                }
                PipeEvent::Disconnected => self.overlay_lost(),
            }
        }
    }

    fn drop_bootstrap(&mut self) {
        self.bootstrap.disconnect();
        self.bootstrap_lost();
    }

    fn bootstrap_lost(&mut self) {
        if self.bootstrap_up {
            emit(&Event::BootstrapDisconnected);
        }
        self.bootstrap_up = false;
        self.bootstrap_hello_due = false;
        for id in self.loads.drain() {
            emit(&Event::reply_error(id, "bootstrap_not_connected"));
        }
    }

    fn drop_overlay(&mut self) {
        self.overlay.disconnect();
        self.overlay_lost();
    }

    fn overlay_lost(&mut self) {
        if self.overlay_up {
            emit(&Event::OverlayDisconnected);
        }
        self.overlay_up = false;
        self.overlay_hello_due = false;
        for id in self.pings.drain() {
            emit(&Event::reply_error(id, "overlay_not_connected"));
        }
    }
}

/// Watch the game process; when it ends, report `game_exited` and exit 0 (design failure table).
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
```

- [ ] **Step 4: Hand the channel to the relay**

In `crates/uw-helper/src/bin/uw-helper64/main.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Finds the game's UI thread and sets a thread hook that
//! makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already pinned, skips
//! the hook), serves the named pipe, and relays JSON lines: events (`bootstrap_connected`,
//! `status`, `error`, …) on stdout, commands (`load_overlay`, …) on stdin, each reply echoing its
//! command's `request_id`; diagnostics on stderr.
//!
//! Modules: `win32` (the real `Win32` for `decide`), `pipe_server` (the overlapped pipe server),
//! `commands` (the stdin reader; stdin closing ends the helper in every phase), `relay` (the
//! JSON-lines relay and the game watcher).

#[cfg(not(windows))]
```

with:

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Creates the control channel (doorbells, mask region,
//! state block, overlay pipe) and the bootstrap pipe, finds the game's UI thread and sets a thread
//! hook that makes Wine load `uw_bootstrap64.dll` into the game (or, if a bootstrap is already
//! pinned, skips the hook), then serves both pipes and relays JSON lines: commands
//! (`load_overlay`, `set_state`, `ping`) on stdin, events on stdout, each reply echoing its
//! command's `request_id`; diagnostics on stderr.
//!
//! Modules: `win32` (the real `Win32` for `decide`, and the channel), `pipe_server` (the
//! overlapped pipe server both pipes use), `commands` (the stdin reader; stdin closing ends the
//! helper in every phase), `relay` (the one wait loop, and the game watcher).

#[cfg(not(windows))]
```

2. Replace:

```rust

    // Read stdin from the start, so its closing ends the helper in every phase.
    let commands = commands::spawn_stdin_reader();
    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
```

with:

```rust

    // Read stdin from the start, so its closing ends the helper in every phase.
    let commands = match commands::spawn_stdin_reader() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[uw-helper64] {e}");
            return 1;
        }
    };
    let mut w = win32::RealWin32::new(exe, bootstrap);
    // The Wine tests override the long default timeouts so `no_window` and `bootstrap_timeout`
```

3. Replace:

```rust
        window_poll_ms: defaults.window_poll_ms,
    };
    let outcome = run(&mut w, &cfg);
    commands.refuse_queued();
```

with:

```rust
        window_poll_ms: defaults.window_poll_ms,
    };
    // The slow-overlay tests shorten the `status` and `ping` timeouts the same way.
    let status_timeout_ms = env_ms("UW_STATUS_TIMEOUT_MS", pipe_server::RELAY_TIMEOUT_MS);
    let ping_timeout_ms = env_ms("UW_PING_TIMEOUT_MS", pipe_server::RELAY_TIMEOUT_MS);
    let outcome = run(&mut w, &cfg);
    commands.refuse_queued();
```

4. Replace:

```rust
        Outcome::Connected { .. } => {
            let pid = w.pid;
            if let Some(pipe) = w.pipe.as_mut() {
                relay::relay(pipe, pid, &commands);
            }
            0
```

with:

```rust
        Outcome::Connected { .. } => {
            let pid = w.pid;
            if let (Some(channel), Some(pipe)) = (w.channel.as_mut(), w.pipe.as_mut()) {
                relay::run(pid, &commands, channel, pipe, status_timeout_ms, ping_timeout_ms);
            }
            0
```


In `crates/uw-helper/src/bin/uw-helper64/win32.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust

/// The control channel: the objects the game opens after its `overlay_hello`, and the overlay
/// pipe it connects to. Held here so both exist from before the hook on; the relay loop (plan 2a
/// Task 7) serves them and removes this `allow`.
#[allow(dead_code)]
pub(crate) struct Channel {
    pub(crate) objects: Objects,
```

with:

```rust

/// The control channel: the objects the game opens after its `overlay_hello`, and the overlay
/// pipe it connects to. Created before the bootstrap pipe; the relay serves both.
pub(crate) struct Channel {
    pub(crate) objects: Objects,
```


- [ ] **Step 5: Both clippies**

Run:
```bash
cargo clippy --target x86_64-pc-windows-gnu -p uw-helper --all-targets -- -D warnings
cargo clippy -p uw-helper --all-targets -- -D warnings
```
Expected: both clean.

- [ ] **Step 6: Update plan 1's `game_exited` test**

The relay now sees the bootstrap pipe drop when the game is killed, so `bootstrap_disconnected` may come just before `game_exited` (both are right; the design lists both events).

In `crates/uw-helper/tests/wine_e2e.rs`, make this edit (each "replace" block is exact and occurs once):

Replace:

```rust
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));

    // Kill the game while the helper is idle on stdin; the watcher thread must notice and report
    // `game_exited` (design failure table), without waiting for the next stdin command.
    if let Some(mut g) = pfx.game.take() {
        let _ = g.kill();
        wait_bounded(&mut g, Duration::from_secs(10));
    }
    let event = helper.next_event(Duration::from_secs(20));
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}
```

with:

```rust
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));

    // Kill the game while the helper is idle; the watcher thread must notice and report
    // `game_exited` (design failure table), without waiting for the next stdin command. The relay
    // may see the bootstrap pipe drop first (`bootstrap_disconnected`); nothing else may come.
    if let Some(mut g) = pfx.game.take() {
        let _ = g.kill();
        wait_bounded(&mut g, Duration::from_secs(10));
    }
    let mut event = helper.next_event(Duration::from_secs(20));
    if event == r#"{"event":"bootstrap_disconnected"}"# {
        event = helper.next_event(Duration::from_secs(20));
    }
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}
```


- [ ] **Step 7: Plan 1's Wine suites stay green**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: helper `7 passed`, `uw-inject` `1 passed`; no leftovers. Before Step 6's edit, `killing_the_game_reports_game_exited` fails with `left: "{\"event\":\"bootstrap_disconnected\"}"`.

- [ ] **Step 8: Commit**

```bash
git add crates/uw-helper
git commit -m "channel: the helper's relay loop over commands, bell-host and both pipes"
```

---

## Task 8: The in-game worker in `uw-game-channel`

Design "Components and flow" (`uw-game-channel`: "A worker thread connects to the overlay pipe (retry 1/s, reconnect on loss), sends `overlay_hello`, opens the doorbells and the state block, delivers bells to a callback, answers `ping`") and "Threads" (only this worker waits on events or the state lock in the game). `start(module_version, on_bells)` spawns it once; the callback gets a `Link` whose `applied(seq)` writes `applied_seq` under the lock and rings `state_applied`. The worker keeps the opened objects and `seen_seq` across reconnects (see the decisions above). It is exercised end to end by Task 9's tests.

**Files:**
- Create: `crates/uw-game-channel/src/worker.rs`
- Modify: `crates/uw-game-channel/src/lib.rs`, `crates/uw-game-channel/Cargo.toml`

- [ ] **Step 1: Write the worker**

`crates/uw-game-channel/src/worker.rs` (complete file):

```rust
//! The in-game worker (design 2a "Components and flow"). One thread, started by the overlay
//! module, for the whole game session:
//! 1. connect to the overlay pipe (retry once a second while it is missing or busy);
//! 2. send `overlay_hello` (module version, state layout);
//! 3. open the doorbells and the state block (`Objects::open`). The previous connection's objects
//!    are dropped only after the new ones are open, so a block the game holds survives a helper
//!    restart (design "Failures");
//! 4. wait on the pipe and `bell-game`: answer `ping` with `pong`, and on a bell swap `to_game`,
//!    read the block under the lock and hand the bits and the values to the overlay's callback;
//! 5. on a lost connection, start again at 1.
//!
//! It is the only thread in the game that waits on events or on the state lock (design
//! "Threads"). A panic, in the callback too, ends only the current connection.

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

use uw_pipe_proto::bells::to_host;
use uw_pipe_proto::names::overlay_pipe_name;
use uw_pipe_proto::pipe::{decode_overlay, encode_overlay, OverlayMessage, HEADER_LEN, MAX_PAYLOAD};
use uw_pipe_proto::state::{self, StateV1, LAYOUT_VERSION};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{SetNamedPipeHandleState, PIPE_READMODE_MESSAGE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, ResetEvent, Sleep, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::objects::{LockError, Objects, LOCK_TIMEOUT_MS};

/// Clients retry once a second without limit (design "Differences from Overwolf").
const RETRY_MS: u32 = 1000;
/// How long one pipe write may take.
const WRITE_TIMEOUT_MS: u32 = 5000;

static STARTED: AtomicBool = AtomicBool::new(false);

/// What the overlay's callback gets: acknowledge an applied state with `applied`.
pub struct Link<'a> {
    objects: &'a Objects,
    seen: &'a Cell<u64>,
}

impl Link<'_> {
    /// The overlay applied the state with this `seq`: write `applied_seq` under the state lock
    /// (design: 100 ms, then `state_busy`), then ring `state_applied`. `ping` answers with the
    /// last `seq` given here.
    pub fn applied(&self, seq: u64) -> Result<(), LockError> {
        let mut g = self.objects.lock(LOCK_TIMEOUT_MS)?;
        state::set_applied_seq(g.bytes_mut(), seq);
        drop(g);
        self.seen.set(seq);
        self.objects.ring_host(to_host::STATE_APPLIED);
        Ok(())
    }
}

/// Start the worker (once per process). `module_version` goes into `overlay_hello`.
/// `on_bells(link, bits, state)` runs on the worker thread for every bell from the host: the
/// `to_game` bits (unknown ones included; ignore them) and the block as read right after.
pub fn start<F>(module_version: u32, on_bells: F) -> std::io::Result<()>
where
    F: FnMut(&Link<'_>, u64, &StateV1) + Send + 'static,
{
    if STARTED.swap(true, Ordering::AcqRel) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "the channel worker already runs"));
    }
    std::thread::Builder::new()
        .name("uw-game-channel".into())
        .spawn(move || worker(module_version, on_bells))
        .map(|_| ())
}

fn worker<F: FnMut(&Link<'_>, u64, &StateV1)>(module_version: u32, mut on_bells: F) {
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    // Kept across connections: see step 3 in the module docs. `seen` is the last `seq` the overlay
    // applied (what `pong` reports); the block it belongs to survives a reconnect too.
    let mut objects: Option<Objects> = None;
    let seen = Cell::new(0u64);
    loop {
        let served =
            catch_unwind(AssertUnwindSafe(|| serve_once(pid, module_version, &mut objects, &seen, &mut on_bells)));
        if served.is_err() {
            eprintln!("[uw-game-channel] serve panicked; reconnecting");
        }
        // SAFETY: a plain sleep.
        unsafe { Sleep(RETRY_MS) };
    }
}

fn serve_once<F: FnMut(&Link<'_>, u64, &StateV1)>(
    pid: u32,
    module_version: u32,
    objects: &mut Option<Objects>,
    seen: &Cell<u64>,
    on_bells: &mut F,
) {
    let mut client = loop {
        match Client::connect(&overlay_pipe_name(pid)) {
            Some(c) => break c,
            // SAFETY: a plain sleep before retrying.
            None => unsafe { Sleep(RETRY_MS) },
        }
    };
    if !client.write(&encode_overlay(&OverlayMessage::Hello { module_version, state_layout: LAYOUT_VERSION })) {
        return;
    }
    match Objects::open(pid) {
        // The old objects drop here, after the new ones are open.
        Ok(o) => *objects = Some(o),
        Err(e) => {
            eprintln!("[uw-game-channel] open: {e}");
            return;
        }
    }
    let o = objects.as_ref().expect("just opened");
    if !client.start_read() {
        return;
    }
    loop {
        let handles = [client.read_event, o.bell_game()];
        // SAFETY: waits on two handles this thread owns; both sources are polled below.
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };

        let bits = o.take_to_game();
        if bits != 0 {
            match o.lock(LOCK_TIMEOUT_MS) {
                Ok(g) => {
                    let s = state::read(g.bytes());
                    drop(g);
                    on_bells(&Link { objects: o, seen }, bits, &s);
                }
                // The helper holds the lock: put the bits back and ring again, so the next pass
                // retries; they are never lost.
                Err(e) => {
                    eprintln!("[uw-game-channel] bell: state lock {e:?}; retrying");
                    o.ring_game(bits);
                }
            }
        }

        match client.poll_read() {
            Read::Pending => {}
            Read::Message(bytes) => match decode_overlay(&bytes) {
                Ok(OverlayMessage::Ping { nonce }) => {
                    if !client.write(&encode_overlay(&OverlayMessage::Pong { nonce, seen_seq: seen.get() }))
                        || !client.start_read()
                    {
                        return;
                    }
                }
                // Anything else from the helper is a bad frame: drop the connection.
                _ => return,
            },
            Read::Broken => return,
        }
    }
}

enum Read {
    Pending,
    Message(Vec<u8>),
    Broken,
}

/// The overlay-pipe client: an overlapped, message-mode handle with one read pending at a time.
/// Dropping it cancels the read (and waits for the cancel) and closes everything, also on unwind.
struct Client {
    pipe: HANDLE,
    read_event: HANDLE,
    write_event: HANDLE,
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    reading: bool,
}

fn manual_event() -> HANDLE {
    // SAFETY: an unnamed, manual-reset, initially unsignaled event.
    unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }
}

impl Client {
    /// `None` while the pipe is missing or busy (both mean "retry later").
    fn connect(name: &str) -> Option<Client> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated name; opens an existing pipe for overlapped duplex I/O.
        let pipe = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return None;
        }
        let client = Client {
            pipe,
            read_event: manual_event(),
            write_event: manual_event(),
            // SAFETY: OVERLAPPED is plain data; all-zero is its initial state.
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; HEADER_LEN + MAX_PAYLOAD],
            reading: false,
        };
        if client.read_event.is_null() || client.write_event.is_null() {
            return None; // `client` drops: handles closed
        }
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: switch our end to message read mode so one read yields one message.
        if unsafe { SetNamedPipeHandleState(client.pipe, &mode, std::ptr::null(), std::ptr::null()) } == 0 {
            return None;
        }
        Some(client)
    }

    /// Start reading the next message. `false`: the pipe is gone.
    fn start_read(&mut self) -> bool {
        // SAFETY: an overlapped read into `buf` (never resized) on the boxed `ov`; both live in
        // `self` until the read completes or `Drop` cancels it.
        let err = unsafe {
            *self.ov = std::mem::zeroed();
            self.ov.hEvent = self.read_event;
            ResetEvent(self.read_event);
            let ok = ReadFile(self.pipe, self.buf.as_mut_ptr(), self.buf.len() as u32, std::ptr::null_mut(), &mut *self.ov);
            if ok != 0 { 0 } else { GetLastError() }
        };
        self.reading = err == 0 || err == ERROR_IO_PENDING;
        self.reading
    }

    fn poll_read(&mut self) -> Read {
        if !self.reading {
            return Read::Broken;
        }
        let mut n = 0u32;
        // SAFETY: collects the pending read's result without waiting.
        unsafe {
            if GetOverlappedResult(self.pipe, &*self.ov, &mut n, 0) != 0 {
                self.reading = false;
                return if n > 0 { Read::Message(self.buf[..n as usize].to_vec()) } else { Read::Broken };
            }
            if GetLastError() == ERROR_IO_INCOMPLETE {
                return Read::Pending;
            }
        }
        self.reading = false;
        Read::Broken
    }

    /// Write one message, waiting at most `WRITE_TIMEOUT_MS`. `false`: the pipe is gone.
    fn write(&self, bytes: &[u8]) -> bool {
        // SAFETY: an overlapped write with its own event; on a timeout it is cancelled and the
        // cancel awaited before `ov` and `bytes` go out of scope.
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = self.write_event;
            ResetEvent(self.write_event);
            let ok = WriteFile(self.pipe, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), &mut ov);
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
            let mut n = 0u32;
            if WaitForSingleObject(self.write_event, WRITE_TIMEOUT_MS) != WAIT_OBJECT_0 {
                CancelIoEx(self.pipe, &ov);
                GetOverlappedResult(self.pipe, &ov, &mut n, 1);
                return false;
            }
            GetOverlappedResult(self.pipe, &ov, &mut n, 0) != 0 && n as usize == bytes.len()
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: cancels our pending read and waits for it before `ov` and `buf` are freed, then
        // closes the handles this client owns.
        unsafe {
            if self.reading {
                CancelIoEx(self.pipe, &*self.ov);
                let mut n = 0u32;
                GetOverlappedResult(self.pipe, &*self.ov, &mut n, 1);
            }
            for h in [self.pipe, self.read_event, self.write_event] {
                if !h.is_null() {
                    CloseHandle(h);
                }
            }
        }
    }
}
```

`crates/uw-game-channel/src/lib.rs` (complete file):

```rust
//! The control channel between the helper and the in-game overlay (design 2a). `objects` holds
//! the per-game named objects: two auto-reset doorbell events, the 16-byte mask region, the state
//! block and its lock. The helper creates them before its pipes; the game only opens them, after
//! its `overlay_hello`. `start` runs the in-game worker an overlay module uses for all of it
//! (`worker`). Windows only.

// The worker relies on `catch_unwind` to survive a panic in the overlay's callback; under
// `panic = "abort"` a panic would kill the game instead.
#[cfg(panic = "abort")]
compile_error!("uw-game-channel needs panic = \"unwind\"");

pub use uw_pipe_proto::bells::{to_game, to_host};
pub use uw_pipe_proto::state::StateV1;

#[cfg(windows)]
pub mod objects;
#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use worker::{start, Link};
```

In `crates/uw-game-channel/Cargo.toml`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```toml
# The control channel's in-game side (design 2a). `objects` holds the named doorbells, mask
# region, state block and lock: the helper creates them (`Objects::create`) and the game only
# opens them (`Objects::open`). Windows only; on Linux the crate is empty. The examples are the
# gate probes run by `tests/wine_objects.rs`.
[lib]
name = "uw_game_channel"
```

with:

```toml
# The control channel's in-game side (design 2a). `objects` holds the named doorbells, mask
# region, state block and lock: the helper creates them (`Objects::create`) and the game only
# opens them (`Objects::open`). `start` runs the in-game worker (overlay pipe, bells, ping). An
# overlay module links it. Windows only; on Linux the crate is empty. The examples are the gate
# probes run by `tests/wine_objects.rs`.
[lib]
name = "uw_game_channel"
```

2. Replace:

```toml

[target.'cfg(windows)'.dependencies]
# `CreateEventW`, `CreateMutexW` and `CreateFileMappingW` take `SECURITY_ATTRIBUTES`, so
# `Win32_Security` is needed even though they get null.
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_Memory",
    "Win32_System_Threading",
] }
```

with:

```toml

[target.'cfg(windows)'.dependencies]
# `CreateEventW`, `CreateMutexW`, `CreateFileMappingW` and `CreateFileW` take
# `SECURITY_ATTRIBUTES`, so `Win32_Security` is needed even though they get null; `ReadFile` and
# `WriteFile` need `Win32_System_IO` for `OVERLAPPED`.
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_Memory",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```


- [ ] **Step 2: Build and both clippies**

Run:
```bash
cargo clippy --target x86_64-pc-windows-gnu -p uw-game-channel --all-targets -- -D warnings
cargo clippy -p uw-game-channel --all-targets -- -D warnings
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
```
Expected: clean, `Finished`.

- [ ] **Step 3: Commit**

```bash
git add crates/uw-game-channel
git commit -m "channel: the in-game worker in uw-game-channel"
```

---

## Task 9: Test overlay on the channel, `--bad-state-block`, and the first end-to-end tests

Design "Components and flow" (`uw-test-overlay`: "Uses `uw-game-channel`; applies state and rings `state_applied`"; `uw-test-game`: `--bad-state-block`). The test overlay starts the worker (module version 1) and, on each bell, writes `uw-test-state-<pid>.txt` and acknowledges the `seq`; `UW_TEST_OVERLAY_START_DELAY_MS` slows its start and `UW_TEST_OVERLAY_BELL_DELAY_MS` every bell (both used in Task 11). The test game's `--bad-state-block` holds a state block with a layout v2 header. The tests first: the overlay connecting, `set_state` → `state_applied` with `applied_seq == seq`, and `ping` → `pong` (design "Testing" 2, first three scenarios). Plan 1's bootstrap-level helper tests now see the overlay's own `overlay_connected` in parallel with the bootstrap's events, so their `next_event` skips the overlay's connect and disconnect events.

**Files:**
- Modify: `crates/uw-helper/tests/wine_e2e.rs`
- Modify: `crates/uw-test-overlay/Cargo.toml`, `crates/uw-test-overlay/src/lib.rs`
- Modify: `crates/uw-test-game/Cargo.toml`, `crates/uw-test-game/src/main.rs`

- [ ] **Step 1: Write the failing end-to-end tests**

In `crates/uw-helper/tests/wine_e2e.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

fn wine() -> String {
```

with:

```rust
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use uw_pipe_proto::jsonl::{Command as Cmd, Event};

fn wine() -> String {
```

2. Replace:

```rust
impl Helper {
    /// The next JSON-lines event, or a test failure (with the helper's stderr) after `within`.
    fn next_event(&self, within: Duration) -> String {
        self.events
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no helper event within {within:?}; stderr: {:?}", self.stderr_lines()))
    }
```

with:

```rust
impl Helper {
    /// The next JSON-lines event, or a test failure (with the helper's stderr) after `within`.
    fn next_any_event(&self, within: Duration) -> String {
        self.events
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no helper event within {within:?}; stderr: {:?}", self.stderr_lines()))
    }

    /// The next event other than the overlay's own `overlay_connected`/`overlay_disconnected`.
    /// Since plan 2a the loaded test overlay connects on its own pipe, in parallel with the
    /// bootstrap's events, and the bootstrap-level tests do not look at it.
    fn next_event(&self, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let event = self.next_any_event(deadline.saturating_duration_since(Instant::now()));
            if !event.contains(r#""event":"overlay_connected""#) && !event.contains(r#""event":"overlay_disconnected""#) {
                return event;
            }
        }
    }
```

3. Replace:

```rust
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }
```

with:

```rust
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next event, parsed (a line that is not an event fails the test).
    fn next_parsed(&self, within: Duration) -> Event {
        let line = self.next_any_event(within);
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"))
    }

    /// Read events until each of `want` has been seen, in any order (two sources, such as the
    /// bootstrap's `status` and the overlay's `overlay_connected`, race). Returns all seen.
    fn expect_all(&self, want: &[Event], within: Duration) -> Vec<Event> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        while !want.iter().all(|w| seen.contains(w)) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "missing {want:?}; saw {seen:?}; stderr: {:?}", self.stderr_lines());
            seen.push(self.next_parsed(left));
        }
        seen
    }

    /// No event at all within `d`.
    fn expect_quiet(&self, d: Duration) {
        if let Ok(line) = self.events.recv_timeout(d) {
            panic!("expected no event, got {line}");
        }
    }

    fn send_cmd(&mut self, cmd: &Cmd) {
        let line = cmd.to_line();
        self.send(&line);
    }
```

4. Replace:

```rust
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}
```

with:

```rust
    assert_eq!(event, r#"{"event":"game_exited"}"#);
}

// --- The control channel (plan 2a) ---

const OVERLAY_VERSION: u32 = 1;

/// The test overlay's last applied state (`uw-test-state-<pid>.txt`), once it has written one.
fn overlay_state(pfx: &Prefix) -> Option<String> {
    let found = markers(pfx, "uw-test-state-");
    found.first().and_then(|p| std::fs::read_to_string(p).ok())
}

fn load(path: &str) -> Cmd {
    Cmd::LoadOverlay { request_id: 1, path: path.into() }
}

fn ok(request_id: u32) -> Event {
    Event::Status { request_id, code: "ok".into(), detail: 0, message: "overlay started".into() }
}

fn applied(applied_seq: u64) -> Event {
    Event::Bell { bits: vec!["state_applied".into()], applied_seq }
}

/// Connect, load the test overlay, and wait for both its `status ok` and its own connection.
fn start_with_overlay(pfx: &Prefix) -> Helper {
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));
    helper
}

/// Catches the design's main flow breaking anywhere: the overlay not connecting on its own pipe,
/// `set_state` not reaching the game through the state block and `bell-game`, the overlay's
/// `state_applied` not coming back through `bell-host`, `applied_seq` not matching `seq`, or an
/// unchanged `set_state` bumping `seq` or ringing.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn the_overlay_connects_and_applies_set_state() {
    let mut pfx = setup("state");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);

    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 2, seq: 1 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(1));
    assert_eq!(overlay_state(&pfx).as_deref(), Some("seq=1 edit_mode=1 visible=0\n"));

    // Nothing changes: the same seq, and no bell.
    helper.send_cmd(&Cmd::SetState { request_id: 3, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 3, seq: 1 });
    helper.expect_quiet(Duration::from_secs(2));

    helper.send_cmd(&Cmd::SetState { request_id: 4, edit_mode: Some(false), visible: Some(true) });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 4, seq: 2 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(2));
    assert_eq!(overlay_state(&pfx).as_deref(), Some("seq=2 edit_mode=0 visible=1\n"));
}

/// Catches `ping` not reaching the overlay, its `pong` not coming back with the request's id and
/// the last applied seq, or a ping with no overlay being sent anyway instead of refused.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn ping_gets_pong_with_the_last_applied_seq() {
    let mut pfx = setup("ping");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));

    // No overlay yet: refused at once (design: no queueing while disconnected).
    helper.send_cmd(&Cmd::Ping { request_id: 9 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::reply_error(9, "overlay_not_connected"));

    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));
    helper.send_cmd(&Cmd::Ping { request_id: 10 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 10, seen_seq: 0 });
    helper.send_cmd(&Cmd::SetState { request_id: 11, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 11, seq: 1 }, applied(1)], Duration::from_secs(10));
    helper.send_cmd(&Cmd::Ping { request_id: 12 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 12, seen_seq: 1 });
}
```


- [ ] **Step 2: Run them to see them fail**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 the_overlay_connects ping_gets_pong
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: both FAIL after about 30 s with `no helper event within 29.9…s` (plan 1's test overlay never opens the channel, so `overlay_connected` never comes); no leftovers.

- [ ] **Step 3: The test overlay uses the channel**

`crates/uw-test-overlay/Cargo.toml` (complete file):

```toml
[package]
name = "uw-test-overlay"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` writes
# `%TEMP%\uw-test-overlay-<pid>.txt`, starts the control channel (`uw-game-channel`) and returns 0;
# on each bell it writes `%TEMP%\uw-test-state-<pid>.txt` and acknowledges the state. The tests
# read both files through the prefix's TEMP. Built for x86_64-pc-windows-gnu.
[lib]
name = "uw_test_overlay"
crate-type = ["cdylib"]

[dependencies]
uw-game-channel = { path = "../uw-game-channel" }

[target.'cfg(windows)'.dependencies]
# `CreateFileW` needs `Win32_Security`, `WriteFile` needs `Win32_System_IO` (see the bootstrap).
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_Threading",
] }
```

`crates/uw-test-overlay/src/lib.rs` (complete file):

```rust
//! A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` proves it ran by writing
//! `%TEMP%\uw-test-overlay-<pid>.txt`, then starts the control channel's worker
//! (`uw_game_channel::start`, module version 1) and returns 0. On every bell it writes
//! `%TEMP%\uw-test-state-<pid>.txt` (`seq=<n> edit_mode=<0|1> visible=<0|1>`) and then
//! acknowledges that `seq` (`applied`), which rings `state_applied`. It ignores `StartParams` (it
//! reads its own pid), which is enough for the tests; the real overlay (plan 3) reads them.
//!
//! In the game's environment, `UW_TEST_OVERLAY_START_DELAY_MS` makes `uw_overlay_start` take that
//! long (the late-reply test), and `UW_TEST_OVERLAY_BELL_DELAY_MS` makes every bell take that long
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetTempPathW, WriteFile, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    const MODULE_VERSION: u32 = 1;

    /// # Safety
    /// Called across the overlay FFI boundary; the pointer is ignored.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn uw_overlay_start(_params: *const core::ffi::c_void) -> u32 {
        // The overlay catches its own panics: a panic must not cross this `extern "system"`
        // boundary (it would abort; the bootstrap caller cannot catch it). 2 = panicked.
        std::panic::catch_unwind(run).unwrap_or(2)
    }

    /// A delay in ms from the environment variable `name`, if set.
    fn env_delay(name: &str) -> Option<std::time::Duration> {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).map(std::time::Duration::from_millis)
    }

    fn run() -> u32 {
        if let Some(d) = env_delay("UW_TEST_OVERLAY_START_DELAY_MS") {
            std::thread::sleep(d);
        }
        let bell_delay = env_delay("UW_TEST_OVERLAY_BELL_DELAY_MS");
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        if !write_temp(&format!("uw-test-overlay-{pid}.txt"), &format!("overlay started in pid {pid}\n")) {
            return 1;
        }
        let started = uw_game_channel::start(MODULE_VERSION, move |link, _bits, state| {
            if let Some(d) = bell_delay {
                std::thread::sleep(d);
            }
            let text = format!("seq={} edit_mode={} visible={}\n", state.seq, state.edit_mode as u8, state.visible as u8);
            write_temp(&format!("uw-test-state-{pid}.txt"), &text);
            if let Err(e) = link.applied(state.seq) {
                eprintln!("[uw-test-overlay] applied: {e:?}");
            }
        });
        if started.is_err() { 3 } else { 0 }
    }

    /// Write `text` to `%TEMP%\<name>` (replacing it). `false` if it could not be created.
    fn write_temp(name: &str, text: &str) -> bool {
        let mut buf = [0u16; 32768];
        // SAFETY: `buf` writable for its length.
        let n = unsafe { GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let mut path: Vec<u16> = buf[..n.min(buf.len())].to_vec();
        path.extend(name.encode_utf16().chain(Some(0)));
        // SAFETY: NUL-terminated path; create and write a small file.
        unsafe {
            let h: HANDLE = CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut written = 0u32;
            let _ = WriteFile(h, text.as_ptr(), text.len() as u32, &mut written, std::ptr::null_mut());
            CloseHandle(h);
        }
        true
    }
}
```

- [ ] **Step 4: `--bad-state-block` in the test game**

`crates/uw-test-game/Cargo.toml` (complete file):

```toml
[package]
name = "uw-test-game"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# A stand-in game for the Wine end-to-end tests: one visible top-level window and a standard
# message loop, so the helper's hook has a UI thread to target. `--no-window` sleeps instead
# (for the `no_window` test); `--bad-state-block` first creates and holds the state block with a
# layout v2 header (for the `version_mismatch` test). Built for x86_64-pc-windows-gnu.
[[bin]]
name = "uw-test-game"
path = "src/main.rs"

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

[target.'cfg(windows)'.dependencies]
# `CreateFileMappingW` takes `SECURITY_ATTRIBUTES`, so `Win32_Security` is needed.
windows-sys = { version = "0.61.2", features = [
    "Win32_Foundation",
    "Win32_Graphics_Gdi",
    "Win32_Security",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_Threading",
    "Win32_UI_WindowsAndMessaging",
] }
```

In `crates/uw-test-game/src/main.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! so a thread-targeted `WH_GETMESSAGE`/`WH_CALLWNDPROC` hook has a UI thread to run on.
//! `uw-test-game --no-window` sleeps with no window (for the `no_window` scenario).

#[cfg(not(windows))]
```

with:

```rust
//! so a thread-targeted `WH_GETMESSAGE`/`WH_CALLWNDPROC` hook has a UI thread to run on.
//! `uw-test-game --no-window` sleeps with no window (for the `no_window` scenario).
//! `uw-test-game --bad-state-block` first creates `Local\underwolf-<pid>-state` with a layout v2
//! header and holds it, as a game would that kept a block from another client version (for the
//! `version_mismatch` scenario).

#[cfg(not(windows))]
```

2. Replace:

```rust
#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--no-window") {
        // No window: the helper must give up with `no_window`.
```

with:

```rust
#[cfg(windows)]
fn main() {
    if std::env::args().any(|a| a == "--bad-state-block") {
        win::hold_bad_state_block();
    }
    if std::env::args().any(|a| a == "--no-window") {
        // No window: the helper must give up with `no_window`.
```

3. Replace:

```rust
    };
    use windows_sys::core::w;

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
```

with:

```rust
    };
    use windows_sys::core::w;

    /// Create the state block for our own pid with a header the helper must refuse: right magic
    /// and size, layout version 2. The handle and view are kept for the life of the process.
    pub fn hold_bad_state_block() {
        use uw_pipe_proto::names::state_name;
        use uw_pipe_proto::state::{init_header, BLOCK_SIZE};
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_WRITE, PAGE_READWRITE};
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;

        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let name: Vec<u16> = state_name(pid).encode_utf16().chain(Some(0)).collect();
        // SAFETY: a named paging-file mapping and a write view of it, both deliberately kept open
        // (never closed or unmapped) until the process exits.
        unsafe {
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, BLOCK_SIZE as u32, name.as_ptr());
            assert!(!h.is_null(), "CreateFileMappingW failed");
            let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 0);
            assert!(!view.Value.is_null(), "MapViewOfFile failed");
            let b = std::slice::from_raw_parts_mut(view.Value.cast::<u8>(), BLOCK_SIZE);
            init_header(b);
            b[4..6].copy_from_slice(&2u16.to_le_bytes()); // layout version 2
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
```


- [ ] **Step 5: Both clippies**

Run:
```bash
cargo clippy --target x86_64-pc-windows-gnu -p uw-test-overlay -p uw-test-game -p uw-helper --all-targets -- -D warnings
cargo clippy -p uw-test-overlay -p uw-test-game -p uw-helper --all-targets -- -D warnings
```
Expected: both clean.

- [ ] **Step 6: Run the helper and `uw-inject` Wine suites**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: helper `9 passed` (plan 1's 7 + `the_overlay_connects_and_applies_set_state` + `ping_gets_pong_with_the_last_applied_seq`), `uw-inject` `1 passed`; no leftovers.

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock crates/uw-helper crates/uw-test-overlay crates/uw-test-game
git commit -m "channel: test overlay on the channel, --bad-state-block, first end-to-end tests"
```

---

## Task 10: `uw-inject` becomes a thin CLI on `uw-host`

Design "Components and flow" (`uw-inject`: "CLI on `uw-host`: JSON commands on stdin (request ids optional, assigned if absent), events on stdout"). The CLI finds the game (printing plan 1's `game_not_found` / `no_wine_session` on failure), starts `Host` with a `WineLaunch`, sends each stdin line, and prints every event. Every command goes out with an id from `HostHandle::next_request_id` (`UserIds::outgoing`, via `Command::parse_with_id`), and a reply gets the user's own id back (`UserIds::incoming`), so a user's id cannot collide with one the host took for its reconcile commands. `Host` gets a `HostHandle` so a stdin thread can send while the main thread receives.

**Files:**
- Modify: `crates/uw-host/src/host.rs`
- Replace: `crates/uw-inject/src/bin/uw-inject.rs`

- [ ] **Step 1: `HostHandle`**

In `crates/uw-host/src/host.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
}

impl Host {
    /// Start supervising; the first helper starts at once.
```

with:

```rust
}

/// Sends commands to a running `Host` from another thread (the `Host` itself receives).
#[derive(Clone)]
pub struct HostHandle {
    inputs: Sender<Input>,
    ids: RequestIds,
}

impl HostHandle {
    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// As `Host::send`.
    pub fn send(&self, cmd: HelperCommand) {
        let _ = self.inputs.send(Input::User(cmd));
    }
}

impl Host {
    /// Start supervising; the first helper starts at once.
```

2. Replace:

```rust
    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// Send a command (it updates the desired state, then goes to the helper, or fails at once
```

with:

```rust
    /// A fresh request id, for a command that has none.
    pub fn next_request_id(&self) -> u32 {
        self.ids.next()
    }

    /// A handle to send commands from another thread.
    pub fn handle(&self) -> HostHandle {
        HostHandle { inputs: self.inputs.clone(), ids: self.ids.clone() }
    }

    /// Send a command (it updates the desired state, then goes to the helper, or fails at once
```


- [ ] **Step 2: Replace the CLI**

`crates/uw-inject/src/bin/uw-inject.rs` (complete file):

```rust
//! Linux launcher: a thin CLI on `uw-host` (design 2a).
//!
//!   uw-inject [--pid N] [--exe deadlock.exe] [--proton DIR] [--helper PATH]
//!             [--bootstrap PATH] [--overlay PATH] [--fallback]
//!
//! Finds the game and its Wine session, then hands over to `uw_host::host::Host`, which starts
//! `uw-helper64.exe` in that session, restarts it if it dies, and keeps the overlay at the desired
//! state (loaded, then every `set_state` it was given). JSON commands on stdin (`request_id`
//! optional: assigned if absent), events on stdout, diagnostics on stderr. The `request_id` you
//! send comes back on your reply; the host's own replies may use the same number. Exits 0 after
//! `stopped {"reason":"game_exited"}` and 1 after any other stop. Defaults: `deadlock.exe`;
//! Proton from the prefix's `config_info`; helper and bootstrap from `~/.local/share/underwolf/`.
//! Ctrl-C (SIGINT) or SIGTERM exits at once: the helper's stdin closes with us, so it exits too
//! (design "Failures"), and the pinned bootstrap stays in the game for the next run.

#[cfg(not(unix))]
fn main() {
    eprintln!("uw-inject is the Linux launcher");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(msg) => {
            eprintln!("[uw-inject] {msg}");
            std::process::exit(1);
        }
    }
}

/// The request ids the user chose, by the id the host gave the command. Every command goes to the
/// host with an id from `HostHandle::next_request_id`, never with the user's own, so a user's id
/// cannot collide with one the host took for its reconcile commands (two pending commands would
/// then share it in the host and the helper); the reply gets the user's id back.
#[cfg(unix)]
#[derive(Default)]
struct UserIds(std::sync::Mutex<std::collections::HashMap<u32, u32>>);

#[cfg(unix)]
impl UserIds {
    /// Parse one stdin line into a command carrying `host_id`; remember the user's id, if any.
    fn outgoing(&self, line: &str, host_id: u32) -> Result<uw_pipe_proto::jsonl::Command, String> {
        let (cmd, user) = uw_pipe_proto::jsonl::Command::parse_with_id(line, host_id).map_err(|e| e.to_string())?;
        if let Some(user) = user {
            self.0.lock().unwrap().insert(host_id, user);
        }
        Ok(cmd)
    }

    /// Put the user's id back on a reply (once: the entry is forgotten).
    fn incoming(&self, event: &mut uw_pipe_proto::jsonl::Event) {
        if let Some(id) = event.request_id_mut()
            && let Some(user) = self.0.lock().unwrap().remove(id)
        {
            *id = user;
        }
    }
}

/// On SIGINT or SIGTERM, exit at once. Only `_exit` (async-signal-safe) happens here; the kernel
/// then closes the helper's stdin, and the helper exits on its own.
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    // SAFETY: `_exit` is async-signal-safe.
    unsafe { libc::_exit(130) };
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::sync::Arc;

    use uw_host::discovery::GameSession;
    use uw_host::driver::Desired;
    use uw_host::host::{Host, WineLaunch};
    use uw_host::to_wine_path;
    use uw_pipe_proto::jsonl::Event;

    let mut pid: Option<i32> = None;
    let mut exe = "deadlock.exe".to_string();
    let mut proton: Option<PathBuf> = None;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let dir = home.join(".local/share/underwolf");
    let mut helper = dir.join("uw-helper64.exe");
    let mut bootstrap = dir.join("uw_bootstrap64.dll");
    let mut overlay: Option<PathBuf> = None;
    let mut fallback = false;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--pid" => pid = Some(value()?.parse().map_err(|e| format!("--pid: {e}"))?),
            "--exe" => exe = value()?,
            "--proton" => proton = Some(value()?.into()),
            "--helper" => helper = value()?.into(),
            "--bootstrap" => bootstrap = value()?.into(),
            "--overlay" => overlay = Some(value()?.into()),
            "--fallback" => fallback = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let session = match GameSession::find_with_proton(&exe, pid, proton) {
        Ok(s) => s,
        Err(e) => {
            // Report the design's Linux-side code on stdout; the detail goes to stderr.
            println!("{}", Event::error(e.code()).to_line());
            eprintln!("[uw-inject] {e}");
            return Ok(1);
        }
    };
    for p in [&helper, &bootstrap] {
        if !p.exists() {
            return Err(format!("missing {}", p.display()));
        }
    }
    if let Some(o) = &overlay
        && !o.exists()
    {
        return Err(format!("missing {}", o.display()));
    }
    eprintln!("[uw-inject] {exe} pid {}; prefix {}", session.pid, session.prefix.display());

    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs a handler that only makes an async-signal-safe call.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    let host = Host::start(
        WineLaunch { session, exe, helper, bootstrap, fallback },
        Desired::new(overlay.as_deref().map(to_wine_path)),
    );
    let commands = host.handle();
    let user_ids = Arc::new(UserIds::default());
    let ids = Arc::clone(&user_ids);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match ids.outgoing(&line, commands.next_request_id()) {
                Ok(cmd) => commands.send(cmd),
                Err(e) => eprintln!("[uw-inject] bad command: {e}"),
            }
        }
        // Our stdin closing does not stop us: Ctrl-C, a signal or a stop does.
    });

    let mut code = 1;
    while let Some(mut event) = host.recv() {
        user_ids.incoming(&mut event);
        println!("{}", event.to_line());
        match &event {
            Event::Error { code, .. } if code == "version_mismatch" => {
                eprintln!("[uw-inject] restart the game to load the new bootstrap or overlay");
            }
            Event::Stopped { reason } => code = if reason == "game_exited" { 0 } else { 1 },
            _ => {}
        }
    }
    Ok(code)
}

#[cfg(all(test, unix))]
mod tests {
    use super::UserIds;
    use uw_pipe_proto::jsonl::{Command, Event};

    /// Catches a user's id reaching the host and the helper: if it equals one the host assigned
    /// (here 1, the host's own reconcile `load_overlay`), two pending commands would share it. The
    /// reply must still carry the user's id.
    #[test]
    fn a_users_id_never_reaches_the_helper_and_comes_back_on_the_reply() {
        let ids = UserIds::default();
        let cmd = ids.outgoing(r#"{"cmd":"ping","request_id":1}"#, 2).unwrap();
        assert_eq!(cmd, Command::Ping { request_id: 2 });
        // The host's own reply keeps its id; the user's comes back as 1, once.
        let mut host_reply = Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() };
        ids.incoming(&mut host_reply);
        assert_eq!(host_reply.request_id(), Some(1));
        let mut pong = Event::Pong { request_id: 2, seen_seq: 0 };
        ids.incoming(&mut pong);
        assert_eq!(pong, Event::Pong { request_id: 1, seen_seq: 0 });
        let mut again = Event::Pong { request_id: 2, seen_seq: 0 };
        ids.incoming(&mut again);
        assert_eq!(again.request_id(), Some(2));
    }

    #[test]
    fn a_command_without_an_id_keeps_the_hosts() {
        let ids = UserIds::default();
        assert_eq!(ids.outgoing(r#"{"cmd":"ping"}"#, 5).unwrap(), Command::Ping { request_id: 5 });
        let mut error = Event::reply_error(5, "overlay_not_connected");
        ids.incoming(&mut error);
        assert_eq!(error, Event::reply_error(5, "overlay_not_connected"));
        assert!(ids.outgoing("not json", 6).is_err());
    }
}
```

- [ ] **Step 3: Unit tests and both clippies**

Run:
```bash
cargo test -p uw-host -p uw-inject
cargo clippy -p uw-host -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-host -p uw-inject --all-targets -- -D warnings
```
Expected: `uw-host` `cli` 3, `discovery` 14, `driver` 4, `host` 3, `supervisor` 8; `uw-inject`'s bin unit tests 2 (`a_users_id_never_reaches_the_helper_and_comes_back_on_the_reply`, `a_command_without_an_id_keeps_the_hosts`); both clippies clean.

- [ ] **Step 4: Plan 1's chain test through the new CLI**

Run:
```bash
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: `1 passed`; no leftovers. Its second `uw-inject` run starts only after the first was SIGTERMed, so it also shows the first helper exited on its own when its stdin closed (otherwise the second gets `already_running`).

- [ ] **Step 5: Commit**

```bash
git add crates/uw-host crates/uw-inject
git commit -m "channel: uw-inject is a thin CLI on uw-host"
```

---

## Task 11: The Wine end-to-end suite for the rest of the design's scenarios

Design "Testing" 2. With Task 9's three, every scenario is covered:
- a slow overlay start → `status_timeout`, the late `status` is discarded, and the next `load_overlay` gets its own reply (`a_slow_overlay_start_times_out_and_its_late_status_is_discarded`);
- `--bad-state-block` → `version_mismatch`, exit 1, no hook (`a_foreign_state_block_is_version_mismatch`);
- closing the helper's stdin → it exits, while it waits for a window and after it connected (`closing_stdin_ends_the_helper_…`);
- kill the helper → `uw-host` restarts it, the overlay reconnects and applies the reconciled state, no reload, `seq` keeps counting, `ping` still answers, the game survives (`a_killed_helper_is_restarted_and_the_overlay_gets_the_reconciled_state`, through `uw-inject`);
- the owner's decision D1: a `ping` the overlay does not answer in time → `overlay_not_responding`, its late `pong` discarded, the next `ping` answered (`a_ping_the_overlay_does_not_answer_in_time_is_overlay_not_responding`; the test overlay's bell delay keeps its worker busy);
- commands sent before the bootstrap connects are refused at once, their replies ahead of `bootstrap_connected` (checked at the start of the slow-start test).

These exercise code from Tasks 6-10, so they pass on their first run; Step 3 checks that they bite.

**Files:**
- Modify: `crates/uw-helper/tests/wine_e2e.rs`, `crates/uw-inject/tests/wine_e2e.rs`

- [ ] **Step 1: Add the tests**

In `crates/uw-helper/tests/wine_e2e.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
            panic!("expected no event, got {line}");
        }
    }
```

with:

```rust
            panic!("expected no event, got {line}");
        }
    }

    /// Close the helper's stdin, as a host that exits or dies does.
    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    /// Wait up to `within` for the helper to exit by itself; its status, or `None` if it did not.
    fn wait_exit(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let start = Instant::now();
        while start.elapsed() < within {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
```

2. Replace:

```rust
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 12, seen_seq: 1 });
}
```

with:

```rust
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 12, seen_seq: 1 });
}

/// Catches plan 1's final-review M2: a `status` that comes after its request timed out must be
/// discarded, not taken as the reply to the next `load_overlay`. Also catches commands sent before
/// the bootstrap connects being queued (and run later) instead of refused at once (design: no
/// queueing while disconnected).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_slow_overlay_start_times_out_and_its_late_status_is_discarded() {
    let mut pfx = setup("slow");
    // The overlay's start takes 6 s; the helper waits only 2 s for a status.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_START_DELAY_MS", "6000").spawn().unwrap(),
    );
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[("UW_STATUS_TIMEOUT_MS", "2000")]);
    // Sent while the helper still hooks: refused, and answered before `bootstrap_connected`.
    helper.send_cmd(&Cmd::LoadOverlay { request_id: 7, path: overlay.clone() });
    helper.send_cmd(&Cmd::SetState { request_id: 8, edit_mode: Some(true), visible: None });
    helper.send_cmd(&Cmd::Ping { request_id: 9 });
    let first: Vec<Event> = (0..4).map(|_| helper.next_parsed(Duration::from_secs(200))).collect();
    assert_eq!(
        first[..3],
        [
            Event::reply_error(7, "bootstrap_not_connected"),
            Event::reply_error(8, "overlay_not_connected"),
            Event::reply_error(9, "overlay_not_connected"),
        ],
        "all: {first:?}"
    );
    assert!(matches!(first[3], Event::BootstrapConnected { overlay_loaded: false, .. }), "{first:?}");

    helper.send_cmd(&load(&overlay));
    let timeout = Event::parse(&helper.next_event(Duration::from_secs(10))).unwrap();
    assert_eq!(timeout, Event::reply_error(1, "status_timeout"));
    // The start finishes about 4 s later; its `status` (request 1) is discarded.
    assert!(
        helper.wait_stderr("discarding a status for request 1", Duration::from_secs(15)),
        "the late status was not discarded; stderr: {:?}",
        helper.stderr_lines()
    );
    // The next request gets its own reply: the overlay did load, so `already_loaded`.
    helper.send_cmd(&Cmd::LoadOverlay { request_id: 2, path: overlay });
    let reply = Event::parse(&helper.next_event(Duration::from_secs(10))).unwrap();
    assert_eq!(
        reply,
        Event::Status { request_id: 2, code: "already_loaded".into(), detail: 0, message: "overlay already loaded".into() }
    );
}

/// Catches owner decision D1 breaking: a `ping` the overlay does not answer in time must get
/// `overlay_not_responding` (and no other reply), its late `pong` must be discarded, and the next
/// `ping` must be answered.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_ping_the_overlay_does_not_answer_in_time_is_overlay_not_responding() {
    let mut pfx = setup("pingslow");
    // Every bell takes the overlay 5 s, and its worker answers no ping meanwhile; the helper
    // waits 2 s for a pong.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_BELL_DELAY_MS", "5000").spawn().unwrap(),
    );
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[("UW_PING_TIMEOUT_MS", "2000")]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));

    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::StateSet { request_id: 2, seq: 1 });
    helper.send_cmd(&Cmd::Ping { request_id: 3 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::reply_error(3, "overlay_not_responding"));
    // The bell is applied about 3 s later; then the late pong comes and is discarded.
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), applied(1));
    assert!(
        helper.wait_stderr("discarding a pong for 3", Duration::from_secs(10)),
        "the late pong was not discarded; stderr: {:?}",
        helper.stderr_lines()
    );
    helper.send_cmd(&Cmd::Ping { request_id: 4 });
    assert_eq!(helper.next_parsed(Duration::from_secs(10)), Event::Pong { request_id: 4, seen_seq: 1 });
}

/// Catches a state block from another layout being used or rewritten instead of refused (design
/// "Messages and layouts": `version_mismatch`, "restart the game"), or the game hooked anyway.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_foreign_state_block_is_version_mismatch() {
    let mut pfx = setup("badblock");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--bad-state-block").spawn().unwrap());
    wait_game_in(&pfx, Duration::from_secs(180));
    let mut helper = start_helper(&pfx, &[], &[]);
    let event = Event::parse(&helper.next_event(Duration::from_secs(60))).unwrap();
    assert_eq!(event, Event::error("version_mismatch"));
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(1), "stderr: {:?}", helper.stderr_lines());
    assert!(
        helper.wait_stderr("state block layout 2, not 1; restart the game", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    assert!(!helper.stderr_lines().iter().any(|l| l.contains("hooked")), "the game was hooked anyway");
}

/// Catches a helper that outlives its host while it still waits for the game's window (design
/// "Failures": it exits when its stdin closes, in every phase, so no orphan keeps the helper mutex).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn closing_stdin_ends_the_helper_while_it_waits_for_a_window() {
    let mut pfx = setup("stdinwait");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).arg("--no-window").spawn().unwrap());
    wait_game_in(&pfx, Duration::from_secs(180));
    // With no window the helper would wait 120 s (the default) before `no_window`.
    let mut helper = start_helper(&pfx, &[], &[]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(helper.child.try_wait().unwrap().is_none(), "the helper ended early: {:?}", helper.stderr_lines());
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "stderr: {:?}", helper.stderr_lines());
    assert!(helper.wait_stderr("stdin closed; exiting", Duration::from_secs(2)));
}

/// The same once the bootstrap is connected and the relay runs.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn closing_stdin_ends_the_helper_after_it_connected() {
    let mut pfx = setup("stdinrelay");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "stderr: {:?}", helper.stderr_lines());
}
```


In `crates/uw-inject/tests/wine_e2e.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! The whole Linux chain under plain Wine: `uw-inject` finds the test game in a scratch prefix,
//! starts `uw-helper64.exe` in that Wine session, the bootstrap connects, and the test overlay is
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. Ignored by
//! default (needs Wine and the Windows artifacts).
//!
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap \
```

with:

```rust
//! The whole Linux chain under plain Wine: `uw-inject` finds the test game in a scratch prefix,
//! starts `uw-helper64.exe` in that Wine session, the bootstrap connects, and the test overlay is
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. A helper
//! killed under a running `uw-inject` is restarted, and the overlay gets the reconciled state.
//! Ignored by default (needs Wine and the Windows artifacts).
//!
//!   cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap \
```

2. Replace:

```rust
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant, SystemTime};

use uw_host::discovery::find_pids;

fn wine() -> String {
```

with:

```rust
//! `UW_WINE`/`UW_WINESERVER` override the Wine binaries (defaults `/usr/bin/wine[server]`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant, SystemTime};

use uw_host::discovery::find_pids;
use uw_pipe_proto::jsonl::Event;

fn wine() -> String {
```

3. Replace:

```rust
    assert_eq!(marker_stamp(&fx.prefix), before, "the overlay marker was rewritten (a second load)");
}
```

with:

```rust
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
```


- [ ] **Step 2: Run both suites**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
cargo clippy -p uw-helper -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-helper -p uw-inject --all-targets -- -D warnings
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: both clippies clean (the edited test files are `#![cfg(unix)]`, so Windows clippy only confirms they compile out); helper `14 passed` (about 70 s), `uw-inject` `2 passed` (about 12 s); no leftovers.

- [ ] **Step 3: Check that the new tests bite, then restore**

Three mutations, each run on its own and then undone with `git checkout`:
1. In `crates/uw-helper/src/bin/uw-helper64/relay.rs`, replace `                        if self.loads.take(request_id) {` with `                        if { self.loads.take(request_id); true } {` (every `status` is forwarded, as before plan 2a).
2. In the same file, delete the three lines `        for id in r.pings.expire(now_ms()) {`, `            emit(&Event::reply_error(id, "overlay_not_responding"));`, `        }` (no ping timeout).
3. In `crates/uw-helper/src/bin/uw-helper64/main.rs`, delete the line `    commands.refuse_queued();` (commands queued until the relay runs).

For each, run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 600 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 a_slow_overlay_start a_ping_the_overlay
git checkout -- crates/uw-helper/src/bin/uw-helper64/relay.rs crates/uw-helper/src/bin/uw-helper64/main.rs
```
Then rebuild and run the two tests once more, and the leftover check:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 600 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 a_slow_overlay_start a_ping_the_overlay
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
```
Expected: mutation 1 fails `a_slow_overlay_start…` with `the late status was not discarded`; mutation 2 fails `a_ping_the_overlay…` with `left: Bell { … applied_seq: 1 }`, `right: Error { request_id: Some(3), code: "overlay_not_responding", … }`; mutation 3 fails `a_slow_overlay_start…` with `left: [BootstrapConnected { … }, …]` (request 7 was queued and run, not refused). After the restore both pass; `git status` shows only this task's test files; no leftovers.

- [ ] **Step 4: Commit**

```bash
git add crates/uw-helper/tests/wine_e2e.rs crates/uw-inject/tests/wine_e2e.rs
git commit -m "channel: Wine end-to-end tests for the design's remaining scenarios"
```

---

## Task 12: Owner live check in Deadlock (Proton)

Design "Testing" 3: "`uw-inject` connects the test overlay; edit mode on and off each give `state_applied`; `ping` → `pong`; the controller kills the helper and all is back within a few seconds; the game keeps running." The controller runs one script, with the owner in the Hideout, and records its output; the implementer only prepares Steps 1-2. Deadlock's Wine process is named `MainThrd`, so it is found by its command line (`pgrep -af 'S:.*deadlock\.exe'`); the escaped dot also keeps a pattern from matching a wrapping shell's own command line. One script, run in one shell, because the steps share `uw-inject`'s stdin (a FIFO held open on fd 3) and its pid.

**Files:** `target/uw-2a-live-check.sh` (not tracked; `target/` is ignored); the result goes into the design doc in Step 4.

- [ ] **Step 1: Build and install the artifacts (implementer)**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-bootstrap -p uw-helper -p uw-test-overlay
cargo build --release -p uw-inject --bin uw-inject
dir="$HOME/.local/share/underwolf"
mkdir -p "$dir"
win="target/x86_64-pc-windows-gnu/release"
# Copy then rename, so a game that already loaded a DLL keeps its copy intact.
for src in "$win/uw_bootstrap64.dll" "$win/uw-helper64.exe" "$win/uw_test_overlay.dll" "target/release/uw-inject"; do
    cp "$src" "$dir/$(basename "$src").tmp" && mv -f "$dir/$(basename "$src").tmp" "$dir/$(basename "$src")"
done
```

- [ ] **Step 2: Write the script (implementer)**

```bash
cat > target/uw-2a-live-check.sh <<'EOF'
#!/usr/bin/env bash
# Plan 2a live check: run with Deadlock in the Hideout. Each step prints under a "==" header.
set -u
D=$(mktemp -d)
T="$HOME/.local/share/Steam/steamapps/compatdata/1422450/pfx/drive_c/users/steamuser/AppData/Local/Temp"
# wait_for SECONDS PATTERN [COUNT]: until PATTERN occurs COUNT (default 1) times in the events.
wait_for() {
    timeout "$1" sh -c 'until [ "$(grep -c -- "$1" "$0")" -ge "$2" ]; do sleep 1; done' "$D/events" "$2" "${3:-1}"
}
echo "== build"; grep ClientVersion "$HOME/.local/share/Steam/steamapps/common/Deadlock/game/citadel/steam.inf"
echo "== game"; pgrep -af 'S:.*deadlock\.exe'
mkfifo "$D/cmd"
exec 3<>"$D/cmd"
"$HOME/.local/share/underwolf/uw-inject" --overlay "$HOME/.local/share/underwolf/uw_test_overlay.dll" \
    <&3 >"$D/events" 2>"$D/stderr" &
INJ=$!
wait_for 120 state_applied && wait_for 10 '"code":"ok"'
echo "== connect"; cat "$D/events"; grep hooked "$D/stderr"

echo '{"cmd":"set_state","edit_mode":true}' >&3; wait_for 10 state_applied 2
echo '{"cmd":"set_state","edit_mode":false}' >&3; wait_for 10 state_applied 3
echo '{"cmd":"ping"}' >&3; wait_for 10 '"event":"pong"'
echo "== edit mode and ping"; tail -n 5 "$D/events"; cat "$T"/uw-test-state-*.txt

echo "== helper"; pgrep -aP "$INJ"
start=$(date +%s)
kill -KILL "$(pgrep -P "$INJ")"
wait_for 60 overlay_connected 2 && wait_for 10 '"event":"state_set"' 4
echo "== back after $(( $(date +%s) - start )) s"
echo '{"cmd":"ping"}' >&3; wait_for 10 '"event":"pong"' 2
echo "== after the kill"; tail -n 5 "$D/events"
echo "status events: $(grep -c '"event":"status"' "$D/events")"
echo "skipped hooks: $(grep -c 'marker present; skipping hook' "$D/stderr")"
echo "== game"; pgrep -af 'S:.*deadlock\.exe'

kill -TERM "$INJ"; sleep 3
echo "== stop"; pgrep -af 'uw-helper64\.exe' || echo "helper gone"
exec 3>&-
echo "== game"; pgrep -af 'S:.*deadlock\.exe'
echo "== logs: $D (remove when done)"
EOF
```

- [ ] **Step 3: Run it (controller, owner in the Hideout)**

**Restart the game before any re-run of this script.** The expected output assumes a game session that no overlay has touched yet: in a session where plan 1's test overlay runs, `bootstrap_connected` says `overlay_loaded: true` and `overlay_connected` never comes; after an earlier run of this script, the state block already says `visible=1`, so the first reconcile rings nothing and the first wait (and every `seq` below) is off. In either case, ask the owner to restart Deadlock and run the script again.

Run: `bash target/uw-2a-live-check.sh`

Expected, section by section:
- `== build`: one line, `ClientVersion=6819` at plan time (record the value).
- `== game`: one line, `<pid> S:\…\deadlock.exe` (record the pid).
- `== connect` (`status` may come anywhere after `bootstrap_connected`):
  ```
  {"event":"bootstrap_connected","overlay_loaded":false}
  {"event":"status","request_id":1,"code":"ok","detail":0,"message":"overlay started"}
  {"event":"overlay_connected","module_version":1}
  {"event":"state_set","request_id":2,"seq":1}
  {"event":"bell","bits":["state_applied"],"applied_seq":1}
  [uw-helper64] hooked WH_GETMESSAGE thread=…
  ```
  (record which hook). If the first event is an `error` (`no_window`, `bootstrap_timeout`), STOP and report to the owner.
- `== edit mode and ping`:
  ```
  {"event":"state_set","request_id":3,"seq":2}
  {"event":"bell","bits":["state_applied"],"applied_seq":2}
  {"event":"state_set","request_id":4,"seq":3}
  {"event":"bell","bits":["state_applied"],"applied_seq":3}
  {"event":"pong","request_id":5,"seen_seq":3}
  seq=3 edit_mode=0 visible=1
  ```
- `== helper`: exactly one line, the helper (`uw-inject`'s only child process).
- `== back after N s`: a few seconds (the first restart waits 1 s).
- `== after the kill` (the state did not change, so no bell):
  ```
  {"event":"helper_restarted","attempt":1}
  {"event":"bootstrap_connected","overlay_loaded":true,"overlay_path":"Z:\\home\\…\\uw_test_overlay.dll"}
  {"event":"overlay_connected","module_version":1}
  {"event":"state_set","request_id":6,"seq":3}
  {"event":"pong","request_id":7,"seen_seq":3}
  status events: 1
  skipped hooks: 1
  ```
- each `== game`: the same pid as the first. **The game keeps running normally throughout.**
- `== stop`: `helper gone` (the helper exited when `uw-inject`'s exit closed its stdin).

- [ ] **Step 4: Record the result**

Add a "Live check (owner)" section at the end of `docs/superpowers/specs/2026-10-23-communication-2a-design.md` with the date, the `ClientVersion`, Proton 11.0, pass or fail, the hook, the "back after" time, and anything unexpected. Then:

```bash
git add docs/superpowers/specs/2026-10-23-communication-2a-design.md
git commit -m "channel: record the Deadlock live-check result"
```

---

## Task 13: Done-check

The design's "Done when": all tests pass, the owner's live check passed, no code under `crates/` calls `CreateRemoteThread`, `WriteProcessMemory` or `VirtualAllocEx`, and clippy `-D warnings` is clean for the host and the Windows target. Verification only.

**Files:** none.

- [ ] **Step 1: No remote-thread injection under `crates/`**

Run: `! grep -rnE 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx' crates/`
Expected: prints nothing, exit 0.

- [ ] **Step 2: All unit tests**

Run: `cargo test -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap`
Expected, all passing: `uw-pipe-proto` names 2, `bells` 3, `jsonl` 8, `pipe` 19, `state` 5; `uw-helper` `decide` 30, `pending` 3; `uw-host` `cli` 3, `discovery` 14, `driver` 4, `host` 3, `supervisor` 8; `uw-inject` bin unit tests 2; `uw-bootstrap` `state` 3. Ignored Wine tests: `uw-game-channel` 1, `uw-helper` 14, `uw-inject` 2.

- [ ] **Step 3: Clippy, host and Windows**

Run:
```bash
cargo clippy --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
cargo metadata --locked --format-version 1 > /dev/null
```
Expected: both clippies clean; `cargo metadata --locked` succeeds (the committed `Cargo.lock` is current).

- [ ] **Step 4: All Windows artifacts and every Wine suite**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; ls -d /tmp/uw-inject-* 2>/dev/null
git status --short
```
Expected: gate `1 passed`, helper `14 passed`, `uw-inject` `2 passed`; the leftover check prints nothing; the tree is clean.

- [ ] **Step 5: The live check passed**

Confirm Task 12's result is recorded in the design doc as a pass. With Steps 1-5 green, plan 2a is done. Nothing to commit.

---

## Self-review (completed by the plan author)

**1. Design coverage.** Every requirement maps to a task:
- Scope "In": the overlay pipe (Tasks 2, 6, 7), the two doorbells and the mask region (Tasks 1, 7, 8), the state block layout v1 (Tasks 1, 7); `uw-game-channel` (Tasks 1, 8); the helper serving both pipes and the objects and relaying them (Tasks 6, 7); `uw-host` (Task 4) and `uw-inject` on it (Task 10); plan 1's deferred items (below); test overlay and test game changes (Task 9).
- Flow steps 1-6: discovery and launch (Task 4, `WineLaunch`); objects, overlay pipe, bootstrap pipe, then hook (Task 6); the overlay's hello, objects, `overlay_connected` (Tasks 7, 8); `set_state` under the lock, `seq` bump, OR the bits, ring `bell-game`, the worker's swap and locked read (Tasks 1, 7, 8); game → host bits, `applied_seq` and `state_applied` (Tasks 7, 8, 9); `ping`/`pong` with the last applied `seq` (Tasks 7, 8).
- Threads: one stdin reader, the game watcher and one wait loop in the helper (Tasks 6, 7); only the worker waits in the game (Task 8).
- Reconcile after every (re)connect (Task 4 `driver`/`supervisor`; Wine: Task 11 restart test).
- Messages and layouts: names with per-kind suffixes (Task 1, uniqueness test), the overlay pipe flags (Task 7, `PipeServer::create`, the same as the bootstrap's), "the game side only opens" (Task 1 `Objects::open`, used only by the worker), bits v1 and `bit<N>` (Tasks 1, 2), layout v1 offsets and the `ERROR_ALREADY_EXISTS` check without rewriting (Tasks 1, 9, 11), overlay messages and the first-message rule (Tasks 2, 7), the JSON commands, replies, unsolicited and `uw-host` events and the error codes (Task 3), no queueing while disconnected (Tasks 4, 6, 7: also for commands that arrive while the helper still hooks), request ids optional on the CLI and never colliding with the host's (Tasks 3, 10).
- Plan 1 items: late reply discarded (Tasks 7, 11), `overlay_path` (Task 3), a failed write is `bootstrap_disconnected` (Tasks 3, 7), `helper_protocol_error` (Task 4), the two Task 3 N1 tests (Task 4).
- Failures table: `overlay_timeout` (Task 4, unit); overlay pipe drop and reconnect (Tasks 7, 8, 11); helper restart back-off and reset, marker skip, reconnect, reconcile (Tasks 4, 11); stdin close in every phase (Tasks 6, 11); game exit stops `uw-host` (Tasks 4, 7); `already_running`/`version_mismatch`/`hook_failed`/`bootstrap_timeout` stop, `no_window`/`channel_failed`/`pipe_error` restart (Task 4, owner decision D2); a `ping` not answered in time is `overlay_not_responding`, its late `pong` discarded (Tasks 7, 11, owner decision D1); `state_busy` at 100 ms (Task 7, `set_state`); `WAIT_ABANDONED` taken as acquired with the header re-checked, and one guard per thread (Task 1, `Objects::lock`, proven in the gate); `pipe_error` (an oversized message too) and listen again (Task 7).
- Testing 1 (unit), 2 (Wine: Tasks 1, 9, 11), 3 (Task 12); "Done when" (Task 13). The open risk is Task 1, a gate.

**2. Placeholder scan.** No TBD/TODO, no "similar to Task N", no step without its code; every run step names its command and expected result. Long files are given whole; small changes are exact replace blocks that occur once in the file as the previous task left it (they were generated from, and checked against, the scratch clone).

**3. Type and name consistency.** `Objects`/`ObjectsError`/`LockError`/`StateGuard`/`LOCK_TIMEOUT_MS` (Task 1) are what Tasks 6-9 use; `OverlayMessage`/`OverlayFirst`/`check_overlay_first`/`bit_names`/`to_game`/`to_host` (Tasks 1-2) match the relay and worker; `Command`/`Event`/`reply_error`/`parse_with_id`/`request_id`/`request_id_mut` (Task 3) match the helper, `uw-host` and `uw-inject`; `ChannelError`/`Outcome::ChannelFailed`/`Win32::create_channel` (Task 6) match the fake and `RealWin32`; `Pending` (Task 7, for loads and pings), `PipeEvent::Oversized`, `Commands::refuse_queued`; `Desired`/`Driver`/`Supervisor`/`STOP_CODES`/`Input`/`Action`/`Host`/`HostHandle`/`Launch`/`WineLaunch`/`RequestIds` (Tasks 4, 10), `UserIds` (Task 10); file names `uw-test-overlay-<pid>.txt`, `uw-test-state-<pid>.txt`; module version 1; prefixes `uw-inject-<tag>-<pid>`.

**Not in this plan, by the design:** the game-data channel (2b), the hotkey table and widget list (layout v2) and input routing (plan 3), a native Windows host, unloading an overlay, a game-thread API beyond the lock-free ring (plan 3's hooks).
