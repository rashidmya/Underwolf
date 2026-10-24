# Communication, Game-Data Channel (Milestone 1, Plan 2b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An in-game producer publishes info keys and events through named shared memory; the Wine-side helper reads them on its own data thread and relays them as JSON lines; `uw-host` passes them on and, after a helper restart, resumes event delivery after the last event it delivered (design `docs/superpowers/specs/2026-10-24-communication-2b-design.md`; spec §6.4 "Data channel (copy Overwolf)" and the relay half of §6.3).

**Architecture:** `uw-pipe-proto::data` holds every byte layout as pure slice code, unit-tested on Linux: the 24-byte hello block, the schema and the info store (per-key timestamps and slots), the 30-record event ring, the resume arithmetic, and the producer's `Outbox` (pending info with its own timestamps, a 256-event FIFO that counts its drops, one throttle of 30 per 200 ms). `uw-game-channel::data` wraps them in Win32 objects named by game pid and channel: `Producer` (in the game: creates every object, tries the locks with 0 ms only) and `Consumer` (in the helper: read-only views, 100 ms lock waits, every read checked). The helper gains a data thread that attaches, emits `data_connected`, the snapshot, `info` and `data_event` lines through 2a's `emit()`, and resumes after `--data-resume`; `uw-host` keeps the resume point per channel and passes it to every restarted helper. The test overlay becomes a test producer (ticks every second, `state_seen` after each applied state), and the test game gains `--bad-data-channel`.

**Tech Stack:** Rust (edition 2024, rustc 1.96.1). Targets `x86_64-unknown-linux-gnu` (host crates, all unit tests) and `x86_64-pc-windows-gnu` (helper, `uw-game-channel`, test overlay, test game). `windows-sys = "0.61.2"`, `serde`/`serde_json`. Wine tests: system Wine 11.20 (`/usr/bin/wine`) under `xvfb-run -a`, `--test-threads=1`, each run bounded by `timeout`.

**Base:** `main` at `e27ea19`: plan 2a's merged code under `crates/` and the owner-approved 2b design. Every snippet below was written against the code in `crates/` now. Every task was built and run in a scratch clone before this plan was written: unit tests, host and Windows clippy (`-D warnings`) and `cargo check --locked` on both targets at every task's commit, the red runs, every mutation step, and the Wine suites (gate 1 + 1, helper 24, `uw-inject` 4) on the final tree, twice, with no processes or prefixes left behind. The plan review's fixes (`plan-review.md`: I1, M1-M8) were verified the same way. The code blocks were then taken from the plan text by a script and replayed onto a fresh clone of `e27ea19`; each task's tree matched the verified one byte for byte. The counts in the expected output come from those runs.

---

## Why these facts (looked up 2026-10-24)

- **`windows-sys` 0.61.2 names**, read in `~/.cargo/registry/src/*/windows-sys-0.61.2/src/Windows/Win32/`:
  - `System/Performance/mod.rs:129-130`: `QueryPerformanceCounter(*mut i64)` and `QueryPerformanceFrequency(*mut i64)`, behind the `Win32_System_Performance` feature (`Cargo.toml:228`).
  - `System/SystemInformation/mod.rs:36`: `GetTickCount64() -> u64` (`Win32_System_SystemInformation`).
  - `System/Threading/mod.rs`: `CreateEventW` (47), `CreateMutexW` (57), `GetCurrentProcessId` (134), `OpenEventW` (238), `OpenMutexW` (239), `ReleaseMutex` (264), `ResetEvent` (270), `SetEvent` (307), `Sleep` (352), `WaitForMultipleObjects` (378); `CreateEventW` and `CreateMutexW` take `SECURITY_ATTRIBUTES`, so `Win32_Security` (already on in every crate that creates objects).
  - `System/Memory/mod.rs`: `CreateFileMappingW` (18), `MapViewOfFile` (65), `OpenFileMappingW` (76, it takes a plain `u32` access), `FILE_MAP_READ = 4`, `FILE_MAP_WRITE = 2` (129, 132).
  - `Foundation/mod.rs`: `ERROR_ALREADY_EXISTS = 183` (1036), `ERROR_FILE_NOT_FOUND = 2` (2220), `WAIT_ABANDONED = 0x80`, `WAIT_FAILED`, `WAIT_OBJECT_0 = 0`, `WAIT_TIMEOUT = 258` (10022-10028).
- **Win32 behaviour:** the design's "Platform facts" (Microsoft Learn): a manual-reset `connect` stays set for a helper that opens it late; `WaitForMultipleObjects` returns the smallest signalled index, so `disconnect` at index 0 wins a tie; a handle must not be closed while a wait on it is pending; `WAIT_ABANDONED` grants ownership; a `FILE_MAP_READ` view cannot be written; paging-file pages start as 0.
- **Wine 11.20, observed in the scratch runs:** a mutex owned by a thread that ends is abandoned and the next waiter gets `WAIT_ABANDONED` (the gate's last phases); the smallest-index rule holds (the gate's `wake info` before `wake events`, and `disconnect` over a stale events bell); a Wine process's stdout pipe fills and its writes block while the reader pauses (the burst test saw 168 records overwritten during a 3 s pause, all announced by `data_lost`).
- **`Option::is_none_or`** is stable since Rust 1.82; used with rustc 1.96.1.
- **`rustfmt --check` on stdin** exits 0 even when it prints a diff (found while checking the new files). The plan checks new files by path: `rustfmt --edition 2024 --check <files>`. Checking `src/data.rs` that way also checks its `data/*.rs` children, which are new too. Plan 2a's files are not rustfmt-clean, so existing files keep their own style (about 120 columns) and are not passed to `rustfmt`.
- **zsh does not word-split `$VAR`** (plan 2a's done-check hit it): every command below lists its packages explicitly.

## File map

| Path | Builds | Responsibility |
|---|---|---|
| `crates/uw-pipe-proto/src/names.rs` | lib (both) | + `DataObject` and `data_name(pid, channel, object)`: `Local\underwolf-<pid>-data-<channel>-<suffix>` |
| `crates/uw-pipe-proto/src/data.rs` | new | The `data` module: `FORMAT_VERSION`, `DEFAULT_CHANNEL`, `ReadError` (`data_version_mismatch` / `data_bad_layout`), byte helpers, re-exports |
| `crates/uw-pipe-proto/src/data/hello.rs` | new | The 24-byte hello block: `write_hello`, `read_hello` (magic, version, the game's pid) |
| `crates/uw-pipe-proto/src/data/schema.rs` | new | `Schema`, `SchemaCategory`, `SchemaKey` (also the `data_connected` shape): build, serialize, parse, the design's limits |
| `crates/uw-pipe-proto/src/data/info.rs` | new | The info store: `InfoLayout`, `info_init`, `info_set` (timestamps only move forward), `InfoReader` (attach, snapshot, changes; every read checked) |
| `crates/uw-pipe-proto/src/data/resume.rs` | new | `ResumePoint` (`<producer_id>:<seq>`), `start_after`, `catch_up`: the pure resume arithmetic |
| `crates/uw-pipe-proto/src/data/ring.rs` | new | The event ring: `ring_init`, `ring_push`, `ring_set_dropped`, `read_ring_head`, `RingReader` (start, drain; lost counts, backwards seqs) |
| `crates/uw-pipe-proto/src/data/outbox.rs` | new | `Outbox` (pending info, the event FIFO, `dropped`), `Throttle`, `ProduceError` |
| `crates/uw-pipe-proto/src/jsonl.rs` | lib | + `data_connected`, `info`, `data_event`, `data_lost`, `data_disconnected`, `Event::data_error` |
| `crates/uw-pipe-proto/tests/data_{schema,info,ring,outbox}.rs` | new tests | The layouts, the rules and the arithmetic |
| `crates/uw-game-channel/src/objects.rs` | Windows | `Handle`, `View` (+ `View::map_as` for read-only views), `wide`, `last_error` shared with `data` |
| `crates/uw-game-channel/src/data.rs` | new, Windows | `Producer` (create, commit, push_event, flush, Drop) and `Consumer` (open, attach, changes, drain, wait) |
| `crates/uw-game-channel/examples/data_{producer,consumer}_probe.rs` | new, Windows | The gate's two command-driven probes |
| `crates/uw-game-channel/tests/wine_data.rs` | new, ignored | The gate: both probes in one Wine prefix |
| `crates/uw-helper/src/data_pass.rs` | new, lib (both) | `Woke`, `Owed`: the reads on each pass, a busy lock's skipped read owed to the next pass |
| `crates/uw-helper/tests/data_pass.rs` | new tests | The reads per wake, and an owed read retried on any wake |
| `crates/uw-helper/src/bin/uw-helper64/data.rs` | new, Windows | The data thread: attach, snapshot, deltas, events, resume, disconnect |
| `crates/uw-helper/src/bin/uw-helper64/main.rs` | bin, Windows | + `--data-channel`, `--data-resume` (usage error: exit 2); starts the data thread with the relay |
| `crates/uw-helper/examples/overlay_probe.rs` | Windows | + `hold-info-lock [ms]`, `break-info-magic` (needs `Win32_System_Memory` in `uw-helper`'s `Cargo.toml`) |
| `crates/uw-helper/tests/wine_e2e.rs` | test, ignored | Data events on their own channel, a stdout pause, and the data scenarios |
| `crates/uw-host/src/supervisor.rs` | lib | + the resume point per channel; `Action::StartHelper { args }` with `--data-resume` |
| `crates/uw-host/src/host.rs` | lib, unix | `Launch::command(args)`; `WineLaunch` appends them |
| `crates/uw-host/tests/{supervisor,host}.rs` | tests | Resume bookkeeping, data events pass through, `--data-resume` reaches a restarted fake helper |
| `crates/uw-inject/tests/wine_e2e.rs` | test, ignored | The data flow through `uw-inject`, a client restart, a helper kill with no gap and no duplicate |
| `crates/uw-test-overlay/src/lib.rs` | test DLL | + the test producer; `UW_TEST_OVERLAY_DATA_RESTART_MS`, `UW_TEST_OVERLAY_BURST` |
| `crates/uw-test-game/src/main.rs` | test exe | + `--bad-data-channel <version\|layout>` |

## Decisions the design leaves open (reviewers: check these)

- **`--bad-data-channel` takes `version` or `layout`** (owner decision, 2026-10-24; the design text now says so). The design's first "Code layout" gave the flag "a hello block with the wrong version", and its "Testing" expected it to give `data_bad_layout`; by the "Failures" table a wrong version is `data_version_mismatch`. Both rows get an end-to-end test: `version` writes format version 2 (`data_version_mismatch`), `layout` a wrong magic (`data_bad_layout`).
- **The producer's rules are pure code.** `Outbox` in `uw-pipe-proto::data` holds the pending info, the FIFO with its `dropped` count and the throttle, so the design's unit tests ("pending keys keep their own timestamps", "the throttle, and queue overflow counted in `dropped`") run on Linux. `uw-game-channel::data::Producer` wraps it with the Win32 objects and the clock.
- **One throttle, asked only under the lock.** `write_events` asks the throttle only once the events lock is held, so a busy lock spends no budget (Spike 3's second throttle spent it twice).
- **A commit with one bad entry is refused whole.** `commit` checks every entry first; an unknown key or a value over its `max_len` fails the call and nothing of that transaction is kept.
- **A key never set changes even to null.** Timestamp 0 means "never set", so setting a never-set key to null gives it a timestamp and it is in the snapshot as null (the test producer's `last_seq` starts that way). A key already null stays unchanged.
- **The dropped baseline is the count at attach** (owner decision, 2026-10-24: drops while no helper runs stay unreported; the design now says so). A helper reports only growth it sees after it attached (the resume point carries no dropped count). Overwritten records are always reported, by seq.
- **Counters that go backwards are a bad layout.** A resume point ahead of the ring's write head under the same producer, a write head behind the last seq read, a slot holding another seq, and a `dropped` count that shrinks are all "a seq that goes backwards under the same producer id": `data_bad_layout`.
- **A duplicate name does not fit a schema** (a category twice, or a key twice in its category): the producer's `Schema::new` refuses it, and the helper's parse is a bad layout.
- **`data_disconnected` only after `data_connected`.** A producer refused at attach (`data_version_mismatch`, `data_bad_layout`) gets no `data_disconnected` when it goes away; one refused after `data_connected` does.
- **A failed wait** (`WAIT_FAILED`, not expected with valid handles) sleeps the timeout and then reads both regions as on a timeout, instead of Spike 3's "disconnect".
- **A read skipped on a busy lock is owed** (the plan review's M1): it is retried on the next pass whatever wakes it, not only on its own bell or a quiet second. With plan 5's collector committing every 50 ms the 1 s timeout never fires, so a skipped drain would otherwise wait for the next event. The rule is pure code (`uw_helper::data_pass`) with its own unit test; no Wine test can reach the difference, because the test producer pairs every commit with an event, so both bells ring on every tick.
- **`uw-host` keeps a resume point per channel** but passes `--data-resume` only for channel 1, the channel a helper reads by default (`uw-host` passes no `--data-channel`). The pure supervisor builds the argument (`Action::StartHelper { args }`), so its unit tests see it; `Launch::command` appends it.
- **The test producer runs on a thread of its own** in the test overlay, not on 2a's channel worker (owner decision, 2026-10-24; the design's wording is corrected), fed by the bell callback through a channel after each `applied`; it flushes every 100 ms.
- **Test aids:** the gate's two command-driven probes; `overlay_probe hold-info-lock` and `break-info-magic`; the test overlay's `UW_TEST_OVERLAY_DATA_RESTART_MS` (drop and re-create the producer: the disconnect test) and `UW_TEST_OVERLAY_BURST` (n events of 1000 bytes before each `state_seen`: the burst test); the helper suite's reader puts data events on a channel of their own (2a's tests never see them) and can pause stdout (a host slow to read).
- **Shared Win32 helpers.** `uw-game-channel::data` reuses 2a's `Handle`, `View`, `wide` and `last_error` from `objects.rs` (made `pub(crate)`, with `View::map_as` for read-only views) instead of a second copy. The crate's `Cargo.toml` comment is corrected on the way (plan 2a's final review: on Linux the crate re-exports types, it is not empty).

## Conventions for every task

- Work only in the repo; never run anything against Deadlock or Steam (Task 11 is run by the controller).
- Wine runs: always `timeout … xvfb-run -a cargo test … -- --ignored --test-threads=1`, the Windows artifacts built first. Afterwards both of these print nothing: `ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep`, and `find /tmp -maxdepth 1 -name 'uw-inject-*'` (no prefix left; `find`, not a glob, so zsh prints no `no matches found`). Never `pkill`/`killall` wine or wineserver: the tests end their own prefix's wineserver only.
- Every task touching Windows code runs `cargo clippy --target x86_64-pc-windows-gnu -p <crate> --all-targets -- -D warnings` (host clippy cannot see `cfg(windows)` code) and the host clippy.
- Every `unsafe` block has a true `// SAFETY:` comment. Comments wrap near 100 columns. New files are rustfmt-clean (`rustfmt --edition 2024 --check <file>`); existing files keep their style.
- Mutation steps come after the commit, so `git checkout -- <file>` restores the committed code.
- Commits: the repo's git user, the message given, no `Co-Authored-By`, no AI or Claude mention, no session links. `Cargo.lock` is committed when it changes.

---

## Task 1: `uw-pipe-proto::data`: the object names, the hello block and the schema

Design "Messages and layouts": the names (`Local\underwolf-<pid>-data-<channel>-<suffix>`, one suffix per object, in `uw-pipe-proto::names` next to 2a's), the 24-byte hello block, and the info schema with the design's limits (names of at most 255 bytes, `max_len` of at most 64 KiB, at most 1024 keys). The `data` module starts here; Tasks 2-4 add the info store, the ring and the producer's outbox to it. `ReadError` is the helper's error for bytes it must refuse: `Version` is `data_version_mismatch`, `Layout` is `data_bad_layout`. Pure code, unit-tested on Linux.

**Files:**
- Modify: `crates/uw-pipe-proto/src/names.rs`, `crates/uw-pipe-proto/src/lib.rs`
- Create: `crates/uw-pipe-proto/src/data.rs`, `crates/uw-pipe-proto/src/data/hello.rs`, `crates/uw-pipe-proto/src/data/schema.rs`, `crates/uw-pipe-proto/tests/data_schema.rs`

- [ ] **Step 1: Write the failing tests**

`crates/uw-pipe-proto/tests/data_schema.rs` (complete file):

```rust
use uw_pipe_proto::data::{
    FORMAT_VERSION, HELLO_SIZE, Hello, MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, ReadError, Schema,
    SchemaCategory, SchemaError, SchemaKey, read_hello, write_hello,
};

fn test_schema() -> Schema {
    Schema::new(vec![
        SchemaCategory::new("game_info", &[("phase", 64)]),
        SchemaCategory::new("match_info", &[("match_id", 32), ("roster_0", 1024)]),
    ])
    .unwrap()
}

/// Catches a hello block written at other offsets than the design's table (magic 0, version 4,
/// pid 8, reserved 12, `producer_id` 16), which the producer and the helper would disagree on.
#[test]
fn the_hello_block_sits_at_the_design_offsets() {
    let mut b = [0xAAu8; HELLO_SIZE];
    write_hello(&mut b, 1234, 0x0102_0304_0506_0708);
    assert_eq!(HELLO_SIZE, 24);
    assert_eq!(&b[0..4], b"UWDH");
    assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 1234);
    assert_eq!(u32::from_le_bytes(b[12..16].try_into().unwrap()), 0);
    assert_eq!(
        u64::from_le_bytes(b[16..24].try_into().unwrap()),
        0x0102_0304_0506_0708
    );
    assert_eq!(FORMAT_VERSION, 1);
    assert_eq!(
        read_hello(&b, 1234),
        Ok(Hello {
            pid: 1234,
            producer_id: 0x0102_0304_0506_0708
        })
    );
}

/// Catches the helper reading a hello block it must refuse (design "Failures"): another version is
/// `data_version_mismatch`; a wrong magic, a short block or another process's pid is
/// `data_bad_layout`.
#[test]
fn a_hello_block_that_does_not_check_out_is_refused() {
    let mut b = [0u8; HELLO_SIZE];
    write_hello(&mut b, 1234, 5);
    let mut v2 = b;
    v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(read_hello(&v2, 1234), Err(ReadError::Version(2)));
    assert_eq!(
        read_hello(&v2, 1234).unwrap_err().code(),
        "data_version_mismatch"
    );

    let mut magic = b;
    magic[3] = b'X';
    let e = read_hello(&magic, 1234).unwrap_err();
    assert_eq!(e, ReadError::Layout("hello block magic is not UWDH".into()));
    assert_eq!(e.code(), "data_bad_layout");
    assert_eq!(
        read_hello(&b, 99),
        Err(ReadError::Layout(
            "hello block pid 1234, not the game's 99".into()
        ))
    );
    assert_eq!(
        read_hello(&b[..16], 1234),
        Err(ReadError::Layout(
            "hello block is 16 bytes, under 24".into()
        ))
    );
    // A block that was created but never written (paging-file pages start as 0).
    assert!(matches!(
        read_hello(&[0u8; HELLO_SIZE], 1234),
        Err(ReadError::Layout(_))
    ));
}

/// Catches a serialized schema that differs from the design's form (u32 category count; per
/// category `u16 len, name, u32 key_count`; per key `u16 len, name, u32 max_len`).
#[test]
fn a_schema_serializes_to_the_design_form_and_parses_back() {
    let s = Schema::new(vec![SchemaCategory::new("ab", &[("k", 7)])]).unwrap();
    let mut want = Vec::new();
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&2u16.to_le_bytes());
    want.extend_from_slice(b"ab");
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&1u16.to_le_bytes());
    want.extend_from_slice(b"k");
    want.extend_from_slice(&7u32.to_le_bytes());
    assert_eq!(s.serialize(), want);
    assert_eq!(Schema::parse(&want), Ok(s));

    let t = test_schema();
    assert_eq!(Schema::parse(&t.serialize()), Ok(t));
}

/// Catches keys numbered other than across categories in order (the info store's slots and
/// timestamps are indexed this way on both sides).
#[test]
fn keys_are_numbered_across_categories_in_order() {
    let s = test_schema();
    assert_eq!(s.key_count(), 3);
    assert_eq!(s.key_index("game_info", "phase"), Some(0));
    assert_eq!(s.key_index("match_info", "match_id"), Some(1));
    assert_eq!(s.key_index("match_info", "roster_0"), Some(2));
    assert_eq!(s.key_index("match_info", "phase"), None);
    assert_eq!(s.key_index("nope", "phase"), None);
    let keys: Vec<_> = s.keys().collect();
    assert_eq!(
        keys,
        [
            ("game_info", "phase", 64),
            ("match_info", "match_id", 32),
            ("match_info", "roster_0", 1024)
        ]
    );
    assert_eq!(
        s.categories()[1].keys[0],
        SchemaKey {
            key: "match_id".into(),
            max_len: 32
        }
    );
}

/// Catches the design's limits not being enforced (names of at most 255 bytes, `max_len` of at
/// most 64 KiB, at most 1024 keys) or a name used twice being accepted.
#[test]
fn a_schema_over_the_limits_is_refused() {
    let long = "n".repeat(MAX_NAME_LEN + 1);
    assert_eq!(
        Schema::new(vec![SchemaCategory::new(&long, &[])]),
        Err(SchemaError::NameTooLong(256))
    );
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[(&long, 1)])]),
        Err(SchemaError::NameTooLong(256))
    );
    assert!(Schema::new(vec![SchemaCategory::new(&"n".repeat(MAX_NAME_LEN), &[])]).is_ok());
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[("k", MAX_VALUE_LEN + 1)])]),
        Err(SchemaError::MaxLenTooLarge {
            key: "k".into(),
            max_len: 65537
        })
    );
    assert!(Schema::new(vec![SchemaCategory::new("c", &[("k", MAX_VALUE_LEN)])]).is_ok());
    let names: Vec<String> = (0..=MAX_KEYS).map(|i| format!("k{i}")).collect();
    let keys: Vec<(&str, u32)> = names.iter().map(|n| (n.as_str(), 1)).collect();
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &keys)]),
        Err(SchemaError::TooManyKeys(1025))
    );
    assert!(Schema::new(vec![SchemaCategory::new("c", &keys[..MAX_KEYS])]).is_ok());
    assert_eq!(
        Schema::new(vec![
            SchemaCategory::new("c", &[]),
            SchemaCategory::new("c", &[])
        ]),
        Err(SchemaError::Duplicate("c".into()))
    );
    assert_eq!(
        Schema::new(vec![SchemaCategory::new("c", &[("k", 1), ("k", 2)])]),
        Err(SchemaError::Duplicate("c.k".into()))
    );
    // The same key name in two categories is two keys.
    assert!(
        Schema::new(vec![
            SchemaCategory::new("a", &[("k", 1)]),
            SchemaCategory::new("b", &[("k", 1)])
        ])
        .is_ok()
    );
}

/// Catches the helper accepting schema bytes that do not fit: cut short, with bytes left over, a
/// name that is not UTF-8, or over a limit.
#[test]
fn schema_bytes_that_do_not_fit_are_refused() {
    let good = test_schema().serialize();
    assert_eq!(
        Schema::parse(&good[..good.len() - 1]),
        Err(SchemaError::Malformed("the schema ends early"))
    );
    let mut extra = good.clone();
    extra.push(0);
    assert_eq!(
        Schema::parse(&extra),
        Err(SchemaError::Malformed("bytes left over after the schema"))
    );
    let mut bad_utf8 = Schema::new(vec![SchemaCategory::new("ab", &[])])
        .unwrap()
        .serialize();
    bad_utf8[6] = 0xFF; // the second byte of "ab"
    assert_eq!(
        Schema::parse(&bad_utf8),
        Err(SchemaError::Malformed("a schema name is not UTF-8"))
    );
    let mut too_big = Schema::new(vec![SchemaCategory::new("c", &[("k", 1)])])
        .unwrap()
        .serialize();
    let n = too_big.len();
    too_big[n - 4..].copy_from_slice(&(MAX_VALUE_LEN + 1).to_le_bytes());
    assert!(matches!(
        Schema::parse(&too_big),
        Err(SchemaError::MaxLenTooLarge { .. })
    ));
    // A huge category count with nothing behind it ends early instead of looping.
    assert_eq!(
        Schema::parse(&u32::MAX.to_le_bytes()),
        Err(SchemaError::Malformed("the schema ends early"))
    );
}
```

In `crates/uw-pipe-proto/src/names.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
    }

    /// Catches two kinds of kernel object sharing a name: the second `Create*W` would then fail
    /// with `ERROR_INVALID_HANDLE` (design "Platform facts").
    #[test]
    fn kernel_object_names_are_all_different() {
        let names = [
            helper_mutex_name(7),
            marker_mutex_name(7),
```

with:

```rust
    }

    /// Catches a data object named off the design's table (`Local\underwolf-<pid>-data-<channel>-
    /// <suffix>`), which the producer and the helper would then disagree on.
    #[test]
    fn data_names_embed_the_pid_the_channel_and_the_design_suffix() {
        let names: Vec<String> = DataObject::ALL
            .iter()
            .map(|&o| data_name(1234, 7, o))
            .collect();
        assert_eq!(
            names,
            [
                r"Local\underwolf-1234-data-7-hello",
                r"Local\underwolf-1234-data-7-info",
                r"Local\underwolf-1234-data-7-events",
                r"Local\underwolf-1234-data-7-info-lock",
                r"Local\underwolf-1234-data-7-events-lock",
                r"Local\underwolf-1234-data-7-info-bell",
                r"Local\underwolf-1234-data-7-events-bell",
                r"Local\underwolf-1234-data-7-connect",
                r"Local\underwolf-1234-data-7-disconnect",
            ]
        );
    }

    /// Catches two kinds of kernel object sharing a name: the second `Create*W` would then fail
    /// with `ERROR_INVALID_HANDLE` (design "Platform facts"). The data objects of channel 1 are
    /// in the list too, so they cannot clash with the control channel's.
    #[test]
    fn kernel_object_names_are_all_different() {
        let mut names = vec![
            helper_mutex_name(7),
            marker_mutex_name(7),
```

2. Replace:

```rust
            state_lock_name(7),
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
```

with:

```rust
            state_lock_name(7),
        ];
        names.extend(DataObject::ALL.iter().map(|&o| data_name(7, 1, o)));
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test data_schema; cargo test -p uw-pipe-proto --lib`
Expected: neither compiles: `` error[E0432]: unresolved import `uw_pipe_proto::data` `` (`` could not find `data` in `uw_pipe_proto` ``) in `data_schema`, and `` error[E0433]: cannot find type `DataObject` in this scope `` and `` error[E0425]: cannot find function `data_name` in this scope `` in `names.rs`'s tests.

- [ ] **Step 3: Add the names, the module, the hello block and the schema**

In `crates/uw-pipe-proto/src/names.rs`, make this edit (each "replace" block is exact and occurs once):

1. Replace:

```rust
}

#[cfg(test)]
mod tests {
```

with:

```rust
}

/// The game-data channel's objects (design 2b "Messages and layouts"). The producer in the game
/// creates all of them; the helper only opens them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataObject {
    /// The hello block (file mapping).
    Hello,
    /// The info store (file mapping).
    Info,
    /// The event ring (file mapping).
    Events,
    /// The info store's lock (mutex).
    InfoLock,
    /// The event ring's lock (mutex).
    EventsLock,
    /// Rung when info changed (auto-reset event).
    InfoBell,
    /// Rung when events were written (auto-reset event).
    EventsBell,
    /// Set once the producer is ready (manual-reset event).
    Connect,
    /// Set when the producer goes away (manual-reset event).
    Disconnect,
}

impl DataObject {
    pub const ALL: [DataObject; 9] = [
        DataObject::Hello,
        DataObject::Info,
        DataObject::Events,
        DataObject::InfoLock,
        DataObject::EventsLock,
        DataObject::InfoBell,
        DataObject::EventsBell,
        DataObject::Connect,
        DataObject::Disconnect,
    ];

    /// The name suffix: one per object, since every kind shares one namespace.
    pub fn suffix(self) -> &'static str {
        match self {
            DataObject::Hello => "hello",
            DataObject::Info => "info",
            DataObject::Events => "events",
            DataObject::InfoLock => "info-lock",
            DataObject::EventsLock => "events-lock",
            DataObject::InfoBell => "info-bell",
            DataObject::EventsBell => "events-bell",
            DataObject::Connect => "connect",
            DataObject::Disconnect => "disconnect",
        }
    }
}

/// `Local\underwolf-<pid>-data-<channel>-<suffix>` (design 2b): the pid keeps two games in one
/// Wine session apart, the channel id lets a later plan add more channels.
pub fn data_name(pid: u32, channel: u32, object: DataObject) -> String {
    format!(r"Local\underwolf-{pid}-data-{channel}-{}", object.suffix())
}

#[cfg(test)]
mod tests {
```

`crates/uw-pipe-proto/src/lib.rs` (complete file):

```rust
//! Underwolf injection and control-channel protocols. `pipe` is the fixed-layout binary framing
//! of the helper's two pipes (bootstrap and overlay, spec 6.3); `jsonl` is the one-object-per-line
//! channel between the Linux host and the helper; `names` is the per-game pipe, mutex, event and
//! mapping names every side must agree on. `bells` (doorbell bits and the mask region) and `state`
//! (the state block, layout v1) are the control channel's shared-memory layouts; `data` is the
//! game-data channel's (design 2b).

pub mod bells;
pub mod data;
pub mod jsonl;
pub mod names;
pub mod pipe;
pub mod state;
```

`crates/uw-pipe-proto/src/data.rs` (complete file):

```rust
//! The game-data channel's layouts (design 2b "Messages and layouts"), as byte-slice code with
//! fixed little-endian offsets, so they are unit-tested on Linux like `state`. The producer in the
//! game creates the objects and writes them; the helper only reads them, and checks every read.
//! The Win32 objects that hold these bytes are `uw_game_channel::data`.
//!
//! - `hello`: the 24-byte hello block (magic, format version, producer pid, `producer_id`).
//! - `schema`: the info store's categories of keys, each key with a maximum value length.

mod hello;
mod schema;

pub use hello::{HELLO_MAGIC, HELLO_SIZE, Hello, read_hello, write_hello};
pub use schema::{
    MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, Schema, SchemaCategory, SchemaError, SchemaKey,
};

/// The format version of the hello block, the info store and the event ring.
pub const FORMAT_VERSION: u32 = 1;
/// The channel milestone 1 uses (design 2b "Scope"); the helper's `--data-channel` defaults to
/// it.
pub const DEFAULT_CHANNEL: u32 = 1;

/// Why bytes the helper read from the channel cannot be used (design 2b "Failures"). The helper
/// reports it once per producer, stops reading and waits for `disconnect`; it is not a stop code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A hello block or a header with another format version (`data_version_mismatch`).
    Version(u32),
    /// Anything else that does not fit (`data_bad_layout`), and why.
    Layout(String),
}

impl ReadError {
    /// The JSON `error` code the helper reports.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::Version(_) => "data_version_mismatch",
            ReadError::Layout(_) => "data_bad_layout",
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Version(v) => write!(f, "format version {v}, not {FORMAT_VERSION}"),
            ReadError::Layout(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for ReadError {}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
```

`crates/uw-pipe-proto/src/data/hello.rs` (complete file):

```rust
//! The hello block (design 2b "Hello block"), 24 bytes, little-endian:
//!
//! | Offset | Field |
//! |---|---|
//! | 0 | magic `UWDH` |
//! | 4 | format version u32 (1) |
//! | 8 | producer pid u32 |
//! | 12 | reserved u32 |
//! | 16 | `producer_id` u64 |
//!
//! The producer writes it last, just before it sets `connect`. `producer_id` is the producer's
//! `GetTickCount64()` at creation: with the pid, it tells one game session from the next.

use super::{FORMAT_VERSION, ReadError, rd_u32, rd_u64, wr_u32, wr_u64};

pub const HELLO_SIZE: usize = 24;
pub const HELLO_MAGIC: [u8; 4] = *b"UWDH";

const OFF_VERSION: usize = 4;
const OFF_PID: usize = 8;
const OFF_RESERVED: usize = 12;
const OFF_PRODUCER_ID: usize = 16;

/// The fields of a hello block that checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub pid: u32,
    pub producer_id: u64,
}

/// The producer's side: write the whole block. Panics if `b` is shorter than `HELLO_SIZE`.
pub fn write_hello(b: &mut [u8], pid: u32, producer_id: u64) {
    b[..4].copy_from_slice(&HELLO_MAGIC);
    wr_u32(b, OFF_VERSION, FORMAT_VERSION);
    wr_u32(b, OFF_PID, pid);
    wr_u32(b, OFF_RESERVED, 0);
    wr_u64(b, OFF_PRODUCER_ID, producer_id);
}

/// The helper's check (design flow step 2): the magic, the format version, and that the
/// producer is the game this helper serves (`game_pid`).
pub fn read_hello(b: &[u8], game_pid: u32) -> Result<Hello, ReadError> {
    if b.len() < HELLO_SIZE {
        return Err(ReadError::Layout(format!(
            "hello block is {} bytes, under {HELLO_SIZE}",
            b.len()
        )));
    }
    if b[..4] != HELLO_MAGIC {
        return Err(ReadError::Layout("hello block magic is not UWDH".into()));
    }
    let version = rd_u32(b, OFF_VERSION);
    if version != FORMAT_VERSION {
        return Err(ReadError::Version(version));
    }
    let pid = rd_u32(b, OFF_PID);
    if pid != game_pid {
        return Err(ReadError::Layout(format!(
            "hello block pid {pid}, not the game's {game_pid}"
        )));
    }
    Ok(Hello {
        pid,
        producer_id: rd_u64(b, OFF_PRODUCER_ID),
    })
}
```

`crates/uw-pipe-proto/src/data/schema.rs` (complete file):

```rust
//! The info schema (design 2b "Info store"): categories of keys, each key with a maximum value
//! length. Keys are numbered across categories in order. Serialized, little-endian, as a u32
//! category count, then per category `u16 len, name, u32 key_count`, and per key
//! `u16 len, name, u32 max_len`.
//!
//! The producer builds it once, at create (`Schema::new`); the helper parses the copy it finds in
//! the info store (`Schema::parse`). Both apply the design's limits: names of at most
//! `MAX_NAME_LEN` bytes, a `max_len` of at most `MAX_VALUE_LEN`, at most `MAX_KEYS` keys. A name
//! used twice (a category, or a key within its category) does not fit either.

use serde::{Deserialize, Serialize};

pub const MAX_NAME_LEN: usize = 255;
pub const MAX_VALUE_LEN: u32 = 64 * 1024;
pub const MAX_KEYS: usize = 1024;

/// One key and the most bytes its value may have. The same shape is in `data_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaKey {
    pub key: String,
    pub max_len: u32,
}

/// One category and its keys, in order. The same shape is in `data_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaCategory {
    pub category: String,
    pub keys: Vec<SchemaKey>,
}

impl SchemaCategory {
    /// `category` with these `(key, max_len)` pairs.
    pub fn new(category: &str, keys: &[(&str, u32)]) -> SchemaCategory {
        SchemaCategory {
            category: category.to_string(),
            keys: keys
                .iter()
                .map(|&(key, max_len)| SchemaKey {
                    key: key.to_string(),
                    max_len,
                })
                .collect(),
        }
    }
}

/// Why a schema does not fit the design's limits, or its bytes do not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    NameTooLong(usize),
    MaxLenTooLarge {
        key: String,
        max_len: u32,
    },
    TooManyKeys(usize),
    /// A category, or a key within its category (`category.key`), named twice.
    Duplicate(String),
    /// Serialized bytes that end early, run on, or hold a name that is not UTF-8.
    Malformed(&'static str),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::NameTooLong(n) => write!(f, "a name of {n} bytes, over {MAX_NAME_LEN}"),
            SchemaError::MaxLenTooLarge { key, max_len } => {
                write!(f, "{key}: max_len {max_len}, over {MAX_VALUE_LEN}")
            }
            SchemaError::TooManyKeys(n) => write!(f, "{n} keys, over {MAX_KEYS}"),
            SchemaError::Duplicate(name) => write!(f, "{name} is named twice"),
            SchemaError::Malformed(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for SchemaError {}

/// A schema that fits the limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    categories: Vec<SchemaCategory>,
    key_count: usize,
}

fn check(categories: &[SchemaCategory]) -> Result<usize, SchemaError> {
    let key_count: usize = categories.iter().map(|c| c.keys.len()).sum();
    if key_count > MAX_KEYS {
        return Err(SchemaError::TooManyKeys(key_count));
    }
    for (i, c) in categories.iter().enumerate() {
        if c.category.len() > MAX_NAME_LEN {
            return Err(SchemaError::NameTooLong(c.category.len()));
        }
        if categories[..i].iter().any(|o| o.category == c.category) {
            return Err(SchemaError::Duplicate(c.category.clone()));
        }
        for (j, k) in c.keys.iter().enumerate() {
            if k.key.len() > MAX_NAME_LEN {
                return Err(SchemaError::NameTooLong(k.key.len()));
            }
            if k.max_len > MAX_VALUE_LEN {
                return Err(SchemaError::MaxLenTooLarge {
                    key: k.key.clone(),
                    max_len: k.max_len,
                });
            }
            if c.keys[..j].iter().any(|o| o.key == k.key) {
                return Err(SchemaError::Duplicate(format!("{}.{}", c.category, k.key)));
            }
        }
    }
    Ok(key_count)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SchemaError> {
        if self.0.len() < n {
            return Err(SchemaError::Malformed("the schema ends early"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, SchemaError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, SchemaError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn name(&mut self) -> Result<String, SchemaError> {
        let n = self.u16()? as usize;
        if n > MAX_NAME_LEN {
            return Err(SchemaError::NameTooLong(n));
        }
        String::from_utf8(self.take(n)?.to_vec())
            .map_err(|_| SchemaError::Malformed("a schema name is not UTF-8"))
    }
}

fn put_name(out: &mut Vec<u8>, s: &str) {
    // `check` keeps every name within MAX_NAME_LEN, so it fits a u16.
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Schema {
    /// The producer's schema, checked against the limits.
    pub fn new(categories: Vec<SchemaCategory>) -> Result<Schema, SchemaError> {
        let key_count = check(&categories)?;
        Ok(Schema {
            categories,
            key_count,
        })
    }

    pub fn categories(&self) -> &[SchemaCategory] {
        &self.categories
    }

    /// Keys in all categories.
    pub fn key_count(&self) -> usize {
        self.key_count
    }

    /// Every key in index order, as `(category, key, max_len)`.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.categories.iter().flat_map(|c| {
            c.keys
                .iter()
                .map(move |k| (c.category.as_str(), k.key.as_str(), k.max_len))
        })
    }

    /// The index of `category.key`, if the schema has it.
    pub fn key_index(&self, category: &str, key: &str) -> Option<usize> {
        let mut base = 0;
        for c in &self.categories {
            if c.category == category {
                return c.keys.iter().position(|k| k.key == key).map(|i| base + i);
            }
            base += c.keys.len();
        }
        None
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.categories.len() as u32).to_le_bytes());
        for c in &self.categories {
            put_name(&mut out, &c.category);
            out.extend_from_slice(&(c.keys.len() as u32).to_le_bytes());
            for k in &c.keys {
                put_name(&mut out, &k.key);
                out.extend_from_slice(&k.max_len.to_le_bytes());
            }
        }
        out
    }

    /// The helper's side: parse exactly `b` (bytes left over do not fit), with the same limits.
    pub fn parse(b: &[u8]) -> Result<Schema, SchemaError> {
        let mut c = Cursor(b);
        let mut categories = Vec::new();
        let mut keys_seen = 0usize;
        for _ in 0..c.u32()? {
            let category = c.name()?;
            let mut keys = Vec::new();
            for _ in 0..c.u32()? {
                keys_seen += 1;
                if keys_seen > MAX_KEYS {
                    return Err(SchemaError::TooManyKeys(keys_seen));
                }
                let key = c.name()?;
                let max_len = c.u32()?;
                keys.push(SchemaKey { key, max_len });
            }
            categories.push(SchemaCategory { category, keys });
        }
        if !c.0.is_empty() {
            return Err(SchemaError::Malformed("bytes left over after the schema"));
        }
        Schema::new(categories)
    }
}
```

- [ ] **Step 4: Run the tests, clippy and rustfmt**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-pipe-proto/src/data.rs crates/uw-pipe-proto/tests/data_schema.rs
```
Expected, all passing: lib unit tests (`names`) 3, `bells` 3, `data_schema` 6, `jsonl` 8, `pipe` 19, `state` 5; both clippies clean; rustfmt prints nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "data: channel names, the hello block and the info schema"
```

- [ ] **Step 6: Check that the pid check bites, then restore**

In `crates/uw-pipe-proto/src/data/hello.rs`, replace `    if pid != game_pid {` with `    if pid != pid {` (any producer's hello block accepted). Run:
```bash
cargo test -p uw-pipe-proto --test data_schema
git checkout -- crates/uw-pipe-proto/src/data/hello.rs
cargo test -p uw-pipe-proto --test data_schema
```
Expected: the first run FAILS `a_hello_block_that_does_not_check_out_is_refused` with `left: Ok(Hello { pid: 1234, producer_id: 5 })`, `right: Err(Layout("hello block pid 1234, not the game's 99"))`; after the restore `6 passed`; `git status` is clean.

---

## Task 2: The info store

Design "Info store": Spike 3's layout with magic `UWDI`, version 1, the 32-byte header (`schema_len` @8, `key_count` @12, `slot_area_off` @16, bytes 20 to 31 reserved; Spike 3's `commit_seq` is dropped), then the schema, one 8-aligned u64 timestamp per key and one slot per key. The producer side (`info_init`, `info_set`) keeps the rules: a key's timestamp only moves forward (`max(ts, prev + 1)`), and an unchanged value keeps its slot and its timestamp. The helper side (`InfoReader`) checks the header on attach and again on every read (it keeps a copy of the header and the schema bytes), every slot length against its `max_len`, and every value for UTF-8: the spec's "stop rather than emit wrong data". Pending values and their own timestamps are Task 4's `Outbox`.

**Files:**
- Modify: `crates/uw-pipe-proto/src/data.rs`
- Create: `crates/uw-pipe-proto/src/data/info.rs`, `crates/uw-pipe-proto/tests/data_info.rs`

- [ ] **Step 1: Write the failing tests**

`crates/uw-pipe-proto/tests/data_info.rs` (complete file):

```rust
use uw_pipe_proto::data::{
    INFO_HEADER_SIZE, InfoLayout, InfoReader, InfoValue, ReadError, Schema, SchemaCategory,
    info_init, info_set,
};

fn schema() -> Schema {
    Schema::new(vec![
        SchemaCategory::new("game", &[("phase", 5)]),
        SchemaCategory::new("match", &[("id", 12), ("score", 9)]),
    ])
    .unwrap()
}

/// A formatted store for `schema()`, its layout and the region.
fn store() -> (Schema, InfoLayout, Vec<u8>) {
    let s = schema();
    let layout = InfoLayout::of(&s);
    let mut region = vec![0xEEu8; layout.size()];
    info_init(&mut region, &s, &layout);
    (s, layout, region)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn value(category: &str, key: &str, v: Option<&str>, ts: u64) -> InfoValue {
    InfoValue {
        category: category.into(),
        key: key.into(),
        value: v.map(str::to_string),
        ts,
    }
}

/// Catches a store laid out differently from the design: the 32-byte header (magic 0, version 4,
/// `schema_len` 8, `key_count` 12, `slot_area_off` 16), the schema right after it, one 8-aligned
/// u64 timestamp per key, then one slot per key (`len` u32 plus `max_len` bytes, padded to 8,
/// null as `u32::MAX`).
#[test]
fn the_store_sits_at_the_design_offsets() {
    let (s, layout, region) = store();
    let schema_bytes = s.serialize();
    assert_eq!(INFO_HEADER_SIZE, 32);
    assert_eq!(&region[0..4], b"UWDI");
    assert_eq!(u32_at(&region, 4), 1);
    assert_eq!(u32_at(&region, 8) as usize, schema_bytes.len());
    assert_eq!(u32_at(&region, 12), 3);
    assert_eq!(&region[20..32], &[0u8; 12], "reserved bytes");
    assert_eq!(&region[32..32 + schema_bytes.len()], &schema_bytes[..]);
    let ts_off = (32 + schema_bytes.len() + 7) & !7;
    let slot_area = ts_off + 3 * 8;
    assert_eq!(u32_at(&region, 16) as usize, slot_area);
    assert_eq!(
        &region[ts_off..slot_area],
        &[0u8; 24],
        "every key starts never set"
    );
    // Slots: phase (4 + 5 -> 16), id (4 + 12 -> 16), score (4 + 9 -> 16).
    for slot in [slot_area, slot_area + 16, slot_area + 32] {
        assert_eq!(u32_at(&region, slot), u32::MAX, "every key starts null");
    }
    assert_eq!(layout.size(), slot_area + 48);
    assert_eq!(
        (layout.max_len(0), layout.max_len(1), layout.max_len(2)),
        (5, 12, 9)
    );
}

/// Catches the snapshot missing a key that was set, holding one that never was, or losing the
/// difference between null and an empty value.
#[test]
fn the_snapshot_holds_every_key_that_was_set_with_nulls() {
    let (_, layout, mut region) = store();
    assert!(info_set(&mut region, &layout, 0, Some("Lobby"), 10));
    assert!(
        info_set(&mut region, &layout, 2, None, 11),
        "a never-set key changes even to null"
    );
    let mut r = InfoReader::attach(&region).unwrap();
    assert_eq!(r.schema(), &schema());
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [
            value("game", "phase", Some("Lobby"), 10),
            value("match", "score", None, 11)
        ]
    );
    // Null and empty are different values.
    assert!(info_set(&mut region, &layout, 2, Some(""), 12));
    assert!(info_set(&mut region, &layout, 2, None, 13));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "score", None, 13)]
    );
    assert!(info_set(&mut region, &layout, 2, Some(""), 14));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "score", Some(""), 14)]
    );
}

/// Catches a timestamp that goes backwards or stays put on a change (the helper would miss the
/// change), or an unchanged value that moves its timestamp (the helper would report a change).
#[test]
fn timestamps_only_move_forward_and_an_unchanged_value_keeps_its_own() {
    let (_, layout, mut region) = store();
    let mut r = InfoReader::attach(&region).unwrap();
    assert!(info_set(&mut region, &layout, 1, Some("m1"), 100));
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("match", "id", Some("m1"), 100)]
    );
    // The producer's clock went back: the timestamp still moves forward.
    assert!(info_set(&mut region, &layout, 1, Some("m2"), 50));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "id", Some("m2"), 101)]
    );
    // The same value again, later: nothing changes, not even the timestamp.
    assert!(!info_set(&mut region, &layout, 1, Some("m2"), 500));
    assert_eq!(r.changes(&region).unwrap(), []);
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("match", "id", Some("m2"), 101)]
    );
    // A value at its max_len fits.
    assert!(info_set(&mut region, &layout, 1, Some("123456789012"), 600));
    assert_eq!(
        r.changes(&region).unwrap(),
        [value("match", "id", Some("123456789012"), 600)]
    );
}

/// Catches `changes` reporting a key twice, or one that did not move since the last read.
#[test]
fn changes_report_only_the_keys_that_moved() {
    let (_, layout, mut region) = store();
    info_set(&mut region, &layout, 0, Some("A"), 1);
    info_set(&mut region, &layout, 1, Some("x"), 1);
    let mut r = InfoReader::attach(&region).unwrap();
    assert_eq!(r.snapshot(&region).unwrap().len(), 2);
    assert_eq!(r.changes(&region).unwrap(), []);
    info_set(&mut region, &layout, 1, Some("y"), 2);
    info_set(&mut region, &layout, 2, Some("3-1"), 2);
    assert_eq!(
        r.changes(&region).unwrap(),
        [
            value("match", "id", Some("y"), 2),
            value("match", "score", Some("3-1"), 2)
        ]
    );
    assert_eq!(r.changes(&region).unwrap(), []);
}

/// Catches the helper attaching to a store whose header does not fit (design "Failures"):
/// another version is `data_version_mismatch`; a wrong magic, a schema that does not parse or
/// runs past the store, a `key_count` or `slot_area_off` that disagrees with the schema, or a
/// store too small for its schema is `data_bad_layout`.
#[test]
fn a_store_whose_header_does_not_fit_is_refused() {
    let (_, _, good) = store();
    let layout_err = |r: &[u8]| match InfoReader::attach(r) {
        Err(ReadError::Layout(why)) => why,
        other => panic!("expected a layout error, got {:?}", other.map(|_| ())),
    };
    let mut b = good.clone();
    b[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(InfoReader::attach(&b).err(), Some(ReadError::Version(2)));

    let mut b = good.clone();
    b[0] = b'X';
    assert_eq!(layout_err(&b), "info store magic is not UWDI");
    let mut b = good.clone();
    b[8..12].copy_from_slice(&100_000u32.to_le_bytes());
    assert_eq!(
        layout_err(&b),
        "info schema of 100000 bytes runs past the store"
    );
    let mut b = good.clone();
    let n = u32_at(&b, 8);
    b[8..12].copy_from_slice(&(n - 1).to_le_bytes());
    assert_eq!(layout_err(&b), "info schema: the schema ends early");
    let mut b = good.clone();
    b[12..16].copy_from_slice(&4u32.to_le_bytes());
    assert_eq!(
        layout_err(&b),
        "info key_count 4, but the schema has 3 keys"
    );
    let mut b = good.clone();
    let off = u32_at(&b, 16);
    b[16..20].copy_from_slice(&(off + 8).to_le_bytes());
    assert_eq!(
        layout_err(&b),
        format!(
            "info slot_area_off {}, but the schema puts it at {off}",
            off + 8
        )
    );
    assert_eq!(
        layout_err(&good[..good.len() - 1]),
        format!(
            "info store is {} bytes, its schema needs {}",
            good.len() - 1,
            good.len()
        )
    );
    assert_eq!(
        layout_err(&good[..16]),
        "info store is 16 bytes, under its header"
    );
    // A mapping that was created but never formatted (paging-file pages start as 0).
    assert_eq!(
        layout_err(&vec![0u8; good.len()]),
        "info store magic is not UWDI"
    );
}

/// Catches a read that trusts what it finds (design: "Every read is checked"): a slot length over
/// its `max_len`, a value that is not UTF-8, or a header changed after attach (for example by an
/// owner that died holding the lock, design "WAIT_ABANDONED") must be `data_bad_layout`, and a
/// version changed after attach `data_version_mismatch`.
#[test]
fn every_read_checks_the_header_and_each_slot() {
    let (_, layout, mut region) = store();
    let slot_area = u32_at(&region, 16) as usize;
    info_set(&mut region, &layout, 0, Some("Lobby"), 1);
    let mut r = InfoReader::attach(&region).unwrap();

    let mut b = region.clone();
    b[slot_area..slot_area + 4].copy_from_slice(&6u32.to_le_bytes());
    assert_eq!(
        r.snapshot(&b),
        Err(ReadError::Layout(
            "game.phase: length 6 over its max_len 5".into()
        ))
    );
    let mut b = region.clone();
    b[slot_area + 4] = 0xFF;
    assert_eq!(
        r.snapshot(&b),
        Err(ReadError::Layout("game.phase: value is not UTF-8".into()))
    );
    let mut b = region.clone();
    b[0] = b'X';
    assert_eq!(
        r.changes(&b),
        Err(ReadError::Layout("info store magic is not UWDI".into()))
    );
    let mut b = region.clone();
    b[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(r.changes(&b), Err(ReadError::Version(2)));
    let mut b = region.clone();
    b[20] = 1; // a reserved byte: still a different header from the one attached
    assert_eq!(
        r.changes(&b),
        Err(ReadError::Layout(
            "info header changed under the same producer".into()
        ))
    );
    // The good store still reads.
    assert_eq!(
        r.snapshot(&region).unwrap(),
        [value("game", "phase", Some("Lobby"), 1)]
    );
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test data_info`
Expected: does not compile: `` error[E0432]: unresolved imports `uw_pipe_proto::data::INFO_HEADER_SIZE`, `uw_pipe_proto::data::InfoLayout`, … `` (all six names).

- [ ] **Step 3: Add the info store**

`crates/uw-pipe-proto/src/data/info.rs` (complete file):

```rust
//! The info store (design 2b "Info store"): one slot per schema key, each with its own timestamp.
//!
//! The header is 32 bytes: magic `UWDI` @0, format version u32 @4, `schema_len` u32 @8,
//! `key_count` u32 @12, `slot_area_off` u32 @16, bytes 20 to 31 reserved. After it come:
//! 1. the serialized schema (`schema_len` bytes);
//! 2. one u64 timestamp per key, 8-aligned (0: never set);
//! 3. one slot per key: `len` u32 plus `max_len` bytes, padded to 8 (`len = u32::MAX`: null).
//!
//! The producer formats the store with `info_init` and writes keys with `info_set`, while it holds
//! the info lock. A key's timestamp only moves forward (`max(ts, prev + 1)`), and an unchanged
//! value keeps its slot and its timestamp. The helper reads with `InfoReader`, also under the
//! lock, and checks the header, every slot length and every value on every read.

use super::{FORMAT_VERSION, ReadError, Schema, rd_u32, rd_u64, wr_u32, wr_u64};

pub const INFO_MAGIC: [u8; 4] = *b"UWDI";
pub const INFO_HEADER_SIZE: usize = 32;

const OFF_VERSION: usize = 4;
const OFF_SCHEMA_LEN: usize = 8;
const OFF_KEY_COUNT: usize = 12;
const OFF_SLOT_AREA: usize = 16;
const NULL: u32 = u32::MAX;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Where everything sits in the info store of one schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoLayout {
    schema_len: usize,
    ts_off: usize,
    slot_area_off: usize,
    /// Per key: the slot's offset and the key's `max_len`.
    slots: Vec<(usize, u32)>,
    size: usize,
}

impl InfoLayout {
    /// The layout of `schema`. The schema's limits keep every offset far below `usize::MAX`
    /// (at most 1024 keys of 64 KiB).
    pub fn of(schema: &Schema) -> InfoLayout {
        let schema_len = schema.serialize().len();
        let ts_off = align8(INFO_HEADER_SIZE + schema_len);
        let slot_area_off = ts_off + 8 * schema.key_count();
        let mut off = slot_area_off;
        let mut slots = Vec::with_capacity(schema.key_count());
        for (_, _, max_len) in schema.keys() {
            slots.push((off, max_len));
            off += align8(4 + max_len as usize);
        }
        InfoLayout {
            schema_len,
            ts_off,
            slot_area_off,
            slots,
            size: off,
        }
    }

    /// The store's size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The `max_len` of key `index`.
    pub fn max_len(&self, index: usize) -> u32 {
        self.slots[index].1
    }

    fn ts_at(&self, index: usize) -> usize {
        self.ts_off + 8 * index
    }
}

/// The producer's side, at create: write the header and the schema, and leave every key never
/// set (timestamp 0) and null. Panics if `region` is smaller than `layout.size()`.
pub fn info_init(region: &mut [u8], schema: &Schema, layout: &InfoLayout) {
    let region = &mut region[..layout.size];
    region.fill(0);
    region[..4].copy_from_slice(&INFO_MAGIC);
    wr_u32(region, OFF_VERSION, FORMAT_VERSION);
    wr_u32(region, OFF_SCHEMA_LEN, layout.schema_len as u32);
    wr_u32(region, OFF_KEY_COUNT, layout.slots.len() as u32);
    wr_u32(region, OFF_SLOT_AREA, layout.slot_area_off as u32);
    region[INFO_HEADER_SIZE..INFO_HEADER_SIZE + layout.schema_len]
        .copy_from_slice(&schema.serialize());
    for &(off, _) in &layout.slots {
        wr_u32(region, off, NULL);
    }
}

/// The producer's side, under the info lock: set key `index` to `value` (`None`: null) at time
/// `ts`. Returns whether the key changed. A key that was never set changes even to null, so it
/// shows up in the helper's snapshot. Panics if `value` is longer than the key's `max_len`
/// (`Outbox::commit` checks that first).
pub fn info_set(
    region: &mut [u8],
    layout: &InfoLayout,
    index: usize,
    value: Option<&str>,
    ts: u64,
) -> bool {
    let (off, max_len) = layout.slots[index];
    let bytes = value.map(str::as_bytes);
    assert!(
        bytes.is_none_or(|v| v.len() <= max_len as usize),
        "info_set: a value over its max_len"
    );
    let ts_at = layout.ts_at(index);
    let prev = rd_u64(region, ts_at);
    let len = rd_u32(region, off);
    let same = prev != 0
        && match bytes {
            None => len == NULL,
            Some(v) => len as usize == v.len() && region[off + 4..off + 4 + v.len()] == *v,
        };
    if same {
        return false;
    }
    match bytes {
        None => wr_u32(region, off, NULL),
        Some(v) => {
            region[off + 4..off + 4 + v.len()].copy_from_slice(v);
            wr_u32(region, off, v.len() as u32);
        }
    }
    wr_u64(region, ts_at, ts.max(prev.saturating_add(1)));
    true
}

/// One key as the helper read it: the snapshot, or a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoValue {
    pub category: String,
    pub key: String,
    /// `None`: the key is null.
    pub value: Option<String>,
    pub ts: u64,
}

/// The helper's side: a checked view of one producer's info store.
pub struct InfoReader {
    schema: Schema,
    layout: InfoLayout,
    names: Vec<(String, String)>,
    /// The header and the schema as attached; every read compares them.
    head: Vec<u8>,
    /// Each key's timestamp at the last snapshot or changes.
    seen: Vec<u64>,
}

impl InfoReader {
    /// Check the store and read its schema (design flow step 2). Another format version is
    /// `Version`; a magic, size, offset or schema that does not fit is `Layout`.
    pub fn attach(region: &[u8]) -> Result<InfoReader, ReadError> {
        let layout_err = |why: String| Err(ReadError::Layout(why));
        if region.len() < INFO_HEADER_SIZE {
            return layout_err(format!(
                "info store is {} bytes, under its header",
                region.len()
            ));
        }
        if region[..4] != INFO_MAGIC {
            return layout_err("info store magic is not UWDI".into());
        }
        let version = rd_u32(region, OFF_VERSION);
        if version != FORMAT_VERSION {
            return Err(ReadError::Version(version));
        }
        let schema_len = rd_u32(region, OFF_SCHEMA_LEN) as usize;
        let Some(schema_bytes) = region[INFO_HEADER_SIZE..].get(..schema_len) else {
            return layout_err(format!(
                "info schema of {schema_len} bytes runs past the store"
            ));
        };
        let schema = match Schema::parse(schema_bytes) {
            Ok(s) => s,
            Err(e) => return layout_err(format!("info schema: {e}")),
        };
        let layout = InfoLayout::of(&schema);
        let key_count = rd_u32(region, OFF_KEY_COUNT) as usize;
        if key_count != schema.key_count() {
            return layout_err(format!(
                "info key_count {key_count}, but the schema has {} keys",
                schema.key_count()
            ));
        }
        let slot_area_off = rd_u32(region, OFF_SLOT_AREA) as usize;
        if slot_area_off != layout.slot_area_off {
            return layout_err(format!(
                "info slot_area_off {slot_area_off}, but the schema puts it at {}",
                layout.slot_area_off
            ));
        }
        if region.len() < layout.size {
            return layout_err(format!(
                "info store is {} bytes, its schema needs {}",
                region.len(),
                layout.size
            ));
        }
        let names = schema
            .keys()
            .map(|(c, k, _)| (c.to_string(), k.to_string()))
            .collect();
        let head = region[..INFO_HEADER_SIZE + schema_len].to_vec();
        Ok(InfoReader {
            seen: vec![0; key_count],
            schema,
            layout,
            names,
            head,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Every key that has been set (a non-zero timestamp), nulls included (design flow step 3).
    /// Later `changes` continue from exactly this snapshot.
    pub fn snapshot(&mut self, region: &[u8]) -> Result<Vec<InfoValue>, ReadError> {
        self.read_where(region, |ts, _| ts != 0)
    }

    /// The keys whose timestamps moved since the last snapshot or changes.
    pub fn changes(&mut self, region: &[u8]) -> Result<Vec<InfoValue>, ReadError> {
        self.read_where(region, |ts, seen| ts != seen)
    }

    fn read_where(
        &mut self,
        region: &[u8],
        wanted: impl Fn(u64, u64) -> bool,
    ) -> Result<Vec<InfoValue>, ReadError> {
        self.check(region)?;
        let mut out = Vec::new();
        let mut seen = self.seen.clone();
        for (i, last) in seen.iter_mut().enumerate() {
            let ts = rd_u64(region, self.layout.ts_at(i));
            if wanted(ts, *last) {
                out.push(self.read(region, i, ts)?);
            }
            *last = ts;
        }
        self.seen = seen;
        Ok(out)
    }

    /// The header and the schema are still what `attach` saw. If not, say why as `attach` would.
    fn check(&self, region: &[u8]) -> Result<(), ReadError> {
        if region.len() >= self.layout.size && region[..self.head.len()] == self.head[..] {
            return Ok(());
        }
        match InfoReader::attach(region) {
            Err(e) => Err(e),
            Ok(_) => Err(ReadError::Layout(
                "info header changed under the same producer".into(),
            )),
        }
    }

    fn read(&self, region: &[u8], i: usize, ts: u64) -> Result<InfoValue, ReadError> {
        let (off, max_len) = self.layout.slots[i];
        let (category, key) = &self.names[i];
        let len = rd_u32(region, off);
        let value = if len == NULL {
            None
        } else if len > max_len {
            return Err(ReadError::Layout(format!(
                "{category}.{key}: length {len} over its max_len {max_len}"
            )));
        } else {
            let bytes = region[off + 4..off + 4 + len as usize].to_vec();
            match String::from_utf8(bytes) {
                Ok(s) => Some(s),
                Err(_) => {
                    return Err(ReadError::Layout(format!(
                        "{category}.{key}: value is not UTF-8"
                    )));
                }
            }
        };
        Ok(InfoValue {
            category: category.clone(),
            key: key.clone(),
            value,
            ts,
        })
    }
}
```

`crates/uw-pipe-proto/src/data.rs` (complete file):

```rust
//! The game-data channel's layouts (design 2b "Messages and layouts"), as byte-slice code with
//! fixed little-endian offsets, so they are unit-tested on Linux like `state`. The producer in the
//! game creates the objects and writes them; the helper only reads them, and checks every read.
//! The Win32 objects that hold these bytes are `uw_game_channel::data`.
//!
//! - `hello`: the 24-byte hello block (magic, format version, producer pid, `producer_id`).
//! - `schema`: the info store's categories of keys, each key with a maximum value length.
//! - `info`: the info store (header, schema, per-key timestamps and value slots).

mod hello;
mod info;
mod schema;

pub use hello::{HELLO_MAGIC, HELLO_SIZE, Hello, read_hello, write_hello};
pub use info::{
    INFO_HEADER_SIZE, INFO_MAGIC, InfoLayout, InfoReader, InfoValue, info_init, info_set,
};
pub use schema::{
    MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, Schema, SchemaCategory, SchemaError, SchemaKey,
};

/// The format version of the hello block, the info store and the event ring.
pub const FORMAT_VERSION: u32 = 1;
/// The channel milestone 1 uses (design 2b "Scope"); the helper's `--data-channel` defaults to
/// it.
pub const DEFAULT_CHANNEL: u32 = 1;

/// Why bytes the helper read from the channel cannot be used (design 2b "Failures"). The helper
/// reports it once per producer, stops reading and waits for `disconnect`; it is not a stop code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A hello block or a header with another format version (`data_version_mismatch`).
    Version(u32),
    /// Anything else that does not fit (`data_bad_layout`), and why.
    Layout(String),
}

impl ReadError {
    /// The JSON `error` code the helper reports.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::Version(_) => "data_version_mismatch",
            ReadError::Layout(_) => "data_bad_layout",
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Version(v) => write!(f, "format version {v}, not {FORMAT_VERSION}"),
            ReadError::Layout(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for ReadError {}

fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
```

- [ ] **Step 4: Run the tests, clippy and rustfmt**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-pipe-proto/src/data.rs crates/uw-pipe-proto/tests/data_info.rs
```
Expected, all passing: lib unit tests 3, `bells` 3, `data_info` 6, `data_schema` 6, `jsonl` 8, `pipe` 19, `state` 5; both clippies clean; rustfmt prints nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "data: the info store layout, writer and checked reader"
```

- [ ] **Step 6: Check that the forward-only rule bites, then restore**

In `crates/uw-pipe-proto/src/data/info.rs`, replace `    wr_u64(region, ts_at, ts.max(prev.saturating_add(1)));` with `    wr_u64(region, ts_at, ts);` (a clock that went back moves the timestamp back). Run:
```bash
cargo test -p uw-pipe-proto --test data_info
git checkout -- crates/uw-pipe-proto/src/data/info.rs
cargo test -p uw-pipe-proto --test data_info
```
Expected: the first run FAILS `timestamps_only_move_forward_and_an_unchanged_value_keeps_its_own` with `left: [InfoValue { …, value: Some("m2"), ts: 50 }]`, `right: [InfoValue { …, value: Some("m2"), ts: 101 }]`; after the restore `6 passed`; `git status` is clean.

---

## Task 3: The event ring and the resume arithmetic

Design "Event ring" and "Failures": a 24-byte header (`UWDE`, version 1, `write_seq` @8, `dropped` @16), then 30 records of `seq`, `ts`, `name_len` + 255 name bytes, `data_len` + 63 KiB of data, as in Spike 3; seqs start at 1 and record `seq` lives in slot `seq % 30`. The pure resume function is split in two: `start_after` picks where a newly attached helper starts (after the resume point if it names this producer, else at the write head), and `catch_up` gives the records to deliver after a seq and how many the ring no longer holds. `RingReader` uses both: its `drain` reports `lost` (overwritten records plus growth in `dropped`) before the events that follow, and treats a seq or a `dropped` count that goes backwards, or a slot holding another seq, as a bad layout instead of Spike 3's resync. `ResumePoint` is `--data-resume <producer_id>:<seq>`, printed by `uw-host` and parsed by the helper.

**Files:**
- Modify: `crates/uw-pipe-proto/src/data.rs`
- Create: `crates/uw-pipe-proto/src/data/resume.rs`, `crates/uw-pipe-proto/src/data/ring.rs`, `crates/uw-pipe-proto/tests/data_ring.rs`

- [ ] **Step 1: Write the failing tests**

`crates/uw-pipe-proto/tests/data_ring.rs` (complete file):

```rust
use uw_pipe_proto::data::{
    CatchUp, Drained, MAX_EVENT_DATA, MAX_EVENT_NAME, RECORD_SIZE, RING_HEADER_SIZE, RING_SIZE,
    RING_SLOTS, ReadError, ResumePoint, RingEvent, RingHead, RingReader, catch_up, read_ring_head,
    ring_init, ring_push, ring_set_dropped, start_after,
};

fn ring() -> Vec<u8> {
    let mut r = vec![0xEEu8; RING_SIZE];
    ring_init(&mut r);
    r
}

fn push_n(r: &mut [u8], n: u64) {
    for _ in 0..n {
        let seq = read_ring_head(r).unwrap().write_seq + 1;
        ring_push(r, "e", &seq.to_string(), seq * 10);
    }
}

fn seqs(d: &Drained) -> Vec<u64> {
    d.events.iter().map(|e| e.seq).collect()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Catches a ring laid out differently from the design: the header (magic 0, version 4,
/// `write_seq` 8, `dropped` 16), 30 records after it, each `seq` 0, `ts` 8, `name_len` 16, the name
/// at 18, `data_len` 276 and the data at 280, record `seq` in slot `seq % 30`.
#[test]
fn the_ring_sits_at_the_design_offsets() {
    let mut r = ring();
    assert_eq!(
        (RING_HEADER_SIZE, RING_SLOTS, RECORD_SIZE),
        (24, 30, 280 + 63 * 1024)
    );
    assert_eq!(RING_SIZE, 24 + 30 * RECORD_SIZE);
    assert_eq!(&r[0..4], b"UWDE");
    assert_eq!(u32::from_le_bytes(r[4..8].try_into().unwrap()), 1);
    assert_eq!((u64_at(&r, 8), u64_at(&r, 16)), (0, 0));
    assert!(r[24..].iter().all(|&b| b == 0), "no records yet");

    assert_eq!(ring_push(&mut r, "tick", "7", 99), 1);
    ring_set_dropped(&mut r, 5);
    let rec = 24 + RECORD_SIZE; // slot 1 % 30
    assert_eq!(u64_at(&r, rec), 1);
    assert_eq!(u64_at(&r, rec + 8), 99);
    assert_eq!(
        u16::from_le_bytes(r[rec + 16..rec + 18].try_into().unwrap()),
        4
    );
    assert_eq!(&r[rec + 18..rec + 22], b"tick");
    assert_eq!(
        u32::from_le_bytes(r[rec + 276..rec + 280].try_into().unwrap()),
        1
    );
    assert_eq!(&r[rec + 280..rec + 281], b"7");
    assert_eq!((u64_at(&r, 8), u64_at(&r, 16)), (1, 5));
    assert_eq!(
        read_ring_head(&r),
        Ok(RingHead {
            write_seq: 1,
            dropped: 5
        })
    );
    // Seq 30 goes to slot 0, seq 31 back to slot 1.
    push_n(&mut r, 30);
    assert_eq!(u64_at(&r, 24), 30);
    assert_eq!(u64_at(&r, rec), 31);
}

/// Catches events delivered out of order, twice, or with their fields mixed up, and the limits
/// (255-byte names, 63 KiB of data) not fitting.
#[test]
fn events_drain_in_seq_order_once_each() {
    let mut r = ring();
    let (mut reader, from) = RingReader::start(&r, 7, None).unwrap();
    assert_eq!(from, 0);
    let long_name = "n".repeat(MAX_EVENT_NAME);
    let big_data = "d".repeat(MAX_EVENT_DATA);
    ring_push(&mut r, "a", "x", 1);
    ring_push(&mut r, &long_name, &big_data, 2);
    ring_push(&mut r, "c", "", 3);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 0);
    assert_eq!(
        d.events,
        [
            RingEvent {
                seq: 1,
                name: "a".into(),
                data: "x".into(),
                ts: 1
            },
            RingEvent {
                seq: 2,
                name: long_name,
                data: big_data,
                ts: 2
            },
            RingEvent {
                seq: 3,
                name: "c".into(),
                data: String::new(),
                ts: 3
            },
        ]
    );
    assert_eq!(reader.after(), 3);
    assert_eq!(
        reader.drain(&r).unwrap(),
        Drained::default(),
        "nothing twice"
    );
}

/// Catches a first attach that replays what the ring still holds (Spike 3's replay, which the
/// design replaces with "start at the write head").
#[test]
fn a_first_attach_starts_at_the_write_head() {
    let mut r = ring();
    push_n(&mut r, 5);
    let (mut reader, from) = RingReader::start(&r, 7, None).unwrap();
    assert_eq!(from, 5);
    assert_eq!(reader.drain(&r).unwrap(), Drained::default());
    push_n(&mut r, 1);
    assert_eq!(seqs(&reader.drain(&r).unwrap()), [6]);
}

/// Catches records overwritten before they were read going uncounted (design `data_lost`), or the
/// oldest ones the ring still holds not being delivered.
#[test]
fn wraparound_before_a_drain_is_counted_as_lost() {
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 40);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 10);
    assert_eq!(seqs(&d), (11..=40).collect::<Vec<_>>());
    push_n(&mut r, 30);
    let d = reader.drain(&r).unwrap();
    assert_eq!(
        (d.lost, d.events.len()),
        (0, 30),
        "exactly a full ring is not a loss"
    );
}

/// Catches growth in the producer's `dropped` count not reaching `data_lost` (design "Failures"),
/// or drops from before the attach being reported as new.
#[test]
fn growth_in_dropped_is_lost_too() {
    let mut r = ring();
    ring_set_dropped(&mut r, 4);
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 2);
    ring_set_dropped(&mut r, 10);
    let d = reader.drain(&r).unwrap();
    assert_eq!((d.lost, seqs(&d)), (6, vec![1, 2]));
    assert_eq!(reader.drain(&r).unwrap().lost, 0);
}

/// Catches a reader that follows a ring whose seq or dropped count goes backwards under the same
/// producer, or a slot holding another seq (design "Failures": `data_bad_layout`, not a resync).
#[test]
fn counters_that_go_backwards_are_a_bad_layout() {
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 5);
    reader.drain(&r).unwrap();
    let mut b = r.clone();
    ring_init(&mut b);
    assert_eq!(
        reader.clone().drain(&b),
        Err(ReadError::Layout(
            "event seq went backwards: the ring is at 0, after 5".into()
        ))
    );
    let mut b = r.clone();
    ring_set_dropped(&mut b, 3);
    let mut grown = reader.clone();
    grown.drain(&b).unwrap();
    ring_set_dropped(&mut b, 1);
    assert_eq!(
        grown.drain(&b),
        Err(ReadError::Layout(
            "the dropped count went backwards: 1 after 3".into()
        ))
    );
    let mut b = r.clone();
    push_n(&mut b, 1);
    let rec6 = RING_HEADER_SIZE + 6 * RECORD_SIZE;
    b[rec6..rec6 + 8].copy_from_slice(&36u64.to_le_bytes());
    assert_eq!(
        reader.clone().drain(&b),
        Err(ReadError::Layout(
            "the ring slot for seq 6 holds seq 36".into()
        ))
    );
}

/// Catches a record or header the helper must not trust (design "Every read is checked"): a name
/// or data length over its limit, text that is not UTF-8, a wrong magic, another version
/// (`data_version_mismatch`), or a ring that is too small.
#[test]
fn records_and_headers_that_do_not_fit_are_refused() {
    let mut r = ring();
    let (reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 1);
    let rec = RING_HEADER_SIZE + RECORD_SIZE;
    let bad = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut b = r.clone();
        edit(&mut b);
        reader.clone().drain(&b).unwrap_err()
    };
    let layout = |why: &str| ReadError::Layout(why.into());
    assert_eq!(
        bad(&|b| b[rec + 16..rec + 18].copy_from_slice(&256u16.to_le_bytes())),
        layout("event 1: name length 256 over 255")
    );
    assert_eq!(
        bad(&|b| b[rec + 276..rec + 280].copy_from_slice(&(64_513u32).to_le_bytes())),
        layout("event 1: data length 64513 over 64512")
    );
    assert_eq!(
        bad(&|b| b[rec + 18] = 0xFF),
        layout("event 1: name is not UTF-8")
    );
    assert_eq!(
        bad(&|b| b[rec + 280] = 0xFF),
        layout("event 1: data is not UTF-8")
    );
    assert_eq!(
        bad(&|b| b[0] = b'X'),
        layout("event ring magic is not UWDE")
    );
    assert_eq!(
        bad(&|b| b[4..8].copy_from_slice(&2u32.to_le_bytes())),
        ReadError::Version(2)
    );
    assert_eq!(
        read_ring_head(&r[..RING_SIZE - 1]),
        Err(layout(&format!(
            "event ring is {} bytes, under {RING_SIZE}",
            RING_SIZE - 1
        )))
    );
    assert_eq!(
        RingReader::start(&vec![0u8; RING_SIZE], 7, None).unwrap_err(),
        layout("event ring magic is not UWDE")
    );
}

/// Catches the resume arithmetic going wrong (design "Failures"): the records after the resume
/// seq, a gap counted when the ring no longer holds them, and a resume point ahead of the ring.
#[test]
fn catch_up_gives_the_records_to_deliver_and_the_gap() {
    assert_eq!(catch_up(0, 0), Ok(CatchUp { lost: 0, first: 1 }));
    assert_eq!(catch_up(0, 30), Ok(CatchUp { lost: 0, first: 1 }));
    assert_eq!(catch_up(0, 31), Ok(CatchUp { lost: 1, first: 2 }));
    assert_eq!(
        catch_up(4, 44),
        Ok(CatchUp {
            lost: 10,
            first: 15
        })
    );
    assert_eq!(catch_up(20, 44), Ok(CatchUp { lost: 0, first: 21 }));
    assert_eq!(catch_up(44, 44), Ok(CatchUp { lost: 0, first: 45 }));
    assert_eq!(
        catch_up(45, 44),
        Err(ReadError::Layout(
            "event seq went backwards: the ring is at 44, after 45".into()
        ))
    );
}

/// Catches a resume that attaches to the wrong producer: a resume point for another producer (a
/// new game, a reused pid) must start at the write head, as a first attach does.
#[test]
fn start_after_resumes_only_the_same_producer() {
    let resume = Some(ResumePoint {
        producer_id: 7,
        seq: 3,
    });
    assert_eq!(start_after(7, 10, resume), 3);
    assert_eq!(start_after(8, 10, resume), 10);
    assert_eq!(start_after(7, 10, None), 10);
}

/// Catches a resume through a real ring going wrong end to end: every event after the resume
/// seq exactly once; an old resume seq gives the gap and then the oldest record; another producer
/// starts at the head; a resume point ahead of the ring is a bad layout.
#[test]
fn a_resumed_reader_delivers_each_event_after_the_resume_seq_once() {
    let mut r = ring();
    push_n(&mut r, 8);
    let (mut reader, from) = RingReader::start(
        &r,
        7,
        Some(ResumePoint {
            producer_id: 7,
            seq: 5,
        }),
    )
    .unwrap();
    assert_eq!(from, 5);
    assert_eq!(seqs(&reader.drain(&r).unwrap()), [6, 7, 8]);

    push_n(&mut r, 40); // the ring is at 48 and holds 19..=48
    let (mut old, from) = RingReader::start(
        &r,
        7,
        Some(ResumePoint {
            producer_id: 7,
            seq: 2,
        }),
    )
    .unwrap();
    assert_eq!(from, 2);
    let d = old.drain(&r).unwrap();
    assert_eq!(d.lost, 16);
    assert_eq!(seqs(&d), (19..=48).collect::<Vec<_>>());

    let (_, from) = RingReader::start(
        &r,
        9,
        Some(ResumePoint {
            producer_id: 7,
            seq: 2,
        }),
    )
    .unwrap();
    assert_eq!(from, 48);
    assert_eq!(
        RingReader::start(
            &r,
            7,
            Some(ResumePoint {
                producer_id: 7,
                seq: 49
            })
        )
        .unwrap_err(),
        ReadError::Layout("event seq went backwards: the ring is at 48, after 49".into())
    );
}

/// Catches `--data-resume` values parsed loosely or printed in another form than
/// `<producer_id>:<seq>` (`uw-host` prints it, the helper parses it).
#[test]
fn a_resume_point_is_producer_id_colon_seq() {
    let p = ResumePoint {
        producer_id: 123_456_789_012,
        seq: 42,
    };
    assert_eq!(p.to_string(), "123456789012:42");
    assert_eq!("123456789012:42".parse(), Ok(p));
    for bad in ["", "1", "1:", ":1", "a:1", "1:b", "1:2:3", "-1:2", "1:2 "] {
        assert!(bad.parse::<ResumePoint>().is_err(), "{bad:?} parsed");
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test data_ring`
Expected: does not compile: `` error[E0432]: unresolved imports `uw_pipe_proto::data::CatchUp`, `uw_pipe_proto::data::Drained`, … `` (all eighteen names).

- [ ] **Step 3: Add the resume arithmetic and the ring**

`crates/uw-pipe-proto/src/data/resume.rs` (complete file):

```rust
//! Where event delivery starts and resumes (design 2b "Failures"), as pure arithmetic on seqs.
//! The helper starts after the resume point `uw-host` gave it when that point names the same
//! producer, and at the write head otherwise. A start that the ring has already overwritten
//! loses the gap, and the rest is delivered once each.

use super::ReadError;

/// The ring's record count: record `seq` lives in slot `seq % RING_SLOTS`.
pub const RING_SLOTS: u64 = 30;

/// `--data-resume <producer_id>:<seq>` (design 2b "Helper arguments"): a producer, and the last
/// seq `uw-host` delivered from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePoint {
    pub producer_id: u64,
    pub seq: u64,
}

impl std::fmt::Display for ResumePoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.producer_id, self.seq)
    }
}

/// Why a `--data-resume` value is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadResumePoint(pub String);

impl std::fmt::Display for BadResumePoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not <producer_id>:<seq>", self.0)
    }
}
impl std::error::Error for BadResumePoint {}

impl std::str::FromStr for ResumePoint {
    type Err = BadResumePoint;

    fn from_str(s: &str) -> Result<ResumePoint, BadResumePoint> {
        let bad = || BadResumePoint(s.to_string());
        let (id, seq) = s.split_once(':').ok_or_else(bad)?;
        Ok(ResumePoint {
            producer_id: id.parse().map_err(|_| bad())?,
            seq: seq.parse().map_err(|_| bad())?,
        })
    }
}

/// The seq after which a newly attached helper delivers events (design flow step 4): the resume
/// point's seq if it names this producer, otherwise the write head (`write_seq`), as on a first
/// attach.
pub fn start_after(producer_id: u64, write_seq: u64, resume: Option<ResumePoint>) -> u64 {
    match resume {
        Some(r) if r.producer_id == producer_id => r.seq,
        _ => write_seq,
    }
}

/// What to deliver after `after`, the last seq delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchUp {
    /// Records overwritten before they could be read: `data_lost`.
    pub lost: u64,
    /// The first record to deliver; the rest follow up to `write_seq`.
    pub first: u64,
}

/// With the ring's last written seq `write_seq`, the records to deliver after `after`:
/// `first..=write_seq`, behind `lost` records the ring no longer holds. A `write_seq` behind
/// `after` is a seq that went backwards under the same producer: a bad layout.
pub fn catch_up(after: u64, write_seq: u64) -> Result<CatchUp, ReadError> {
    if after > write_seq {
        return Err(ReadError::Layout(format!(
            "event seq went backwards: the ring is at {write_seq}, after {after}"
        )));
    }
    let oldest = write_seq.saturating_sub(RING_SLOTS - 1).max(1);
    let next = after.saturating_add(1);
    let first = next.max(oldest);
    Ok(CatchUp {
        lost: first - next,
        first,
    })
}
```

`crates/uw-pipe-proto/src/data/ring.rs` (complete file):

```rust
//! The event ring (design 2b "Event ring"): a 24-byte header, then `RING_SLOTS` records.
//!
//! - Header: magic `UWDE` @0, format version u32 @4, `write_seq` u64 @8 (the last seq written,
//!   0 for none), `dropped` u64 @16 (events the producer's queue dropped).
//! - Record: `seq` u64 @0, `ts` u64 @8, `name_len` u16 @16, the name (up to 255 bytes) @18,
//!   `data_len` u32 @276, the data (up to 63 KiB) @280.
//!
//! Seqs start at 1, and record `seq` lives in slot `seq % RING_SLOTS`. The producer writes a
//! record and then `write_seq`, under the events lock. The helper reads with `RingReader`, under
//! the same lock, and checks the header and every record it reads.

use super::{
    FORMAT_VERSION, RING_SLOTS, ReadError, ResumePoint, catch_up, rd_u16, rd_u32, rd_u64,
    start_after, wr_u16, wr_u32, wr_u64,
};

pub const RING_MAGIC: [u8; 4] = *b"UWDE";
pub const RING_HEADER_SIZE: usize = 24;
pub const MAX_EVENT_NAME: usize = 255;
pub const MAX_EVENT_DATA: usize = 63 * 1024;
/// One record's size: 280 bytes of fields and name, then the data.
pub const RECORD_SIZE: usize = REC_DATA + MAX_EVENT_DATA;
/// The ring's size in bytes (about 1.85 MiB).
pub const RING_SIZE: usize = RING_HEADER_SIZE + RING_SLOTS as usize * RECORD_SIZE;

const OFF_VERSION: usize = 4;
const OFF_WRITE_SEQ: usize = 8;
const OFF_DROPPED: usize = 16;
const REC_SEQ: usize = 0;
const REC_TS: usize = 8;
const REC_NAME_LEN: usize = 16;
const REC_NAME: usize = 18;
const REC_DATA_LEN: usize = 276;
const REC_DATA: usize = 280;

fn slot(seq: u64) -> usize {
    RING_HEADER_SIZE + (seq % RING_SLOTS) as usize * RECORD_SIZE
}

/// The producer's side, at create: the header, no records. Panics if `region` is smaller than
/// `RING_SIZE`.
pub fn ring_init(region: &mut [u8]) {
    let region = &mut region[..RING_SIZE];
    region.fill(0);
    region[..4].copy_from_slice(&RING_MAGIC);
    wr_u32(region, OFF_VERSION, FORMAT_VERSION);
}

/// The producer's side, under the events lock: write the next record and then `write_seq`.
/// Returns the record's seq. Panics if the name or the data is over its limit
/// (`Outbox::push_event` checks that first).
pub fn ring_push(region: &mut [u8], name: &str, data: &str, ts: u64) -> u64 {
    assert!(
        name.len() <= MAX_EVENT_NAME && data.len() <= MAX_EVENT_DATA,
        "ring_push: a name or data over its limit"
    );
    let seq = rd_u64(region, OFF_WRITE_SEQ) + 1;
    let rec = &mut region[slot(seq)..slot(seq) + RECORD_SIZE];
    wr_u64(rec, REC_SEQ, seq);
    wr_u64(rec, REC_TS, ts);
    wr_u16(rec, REC_NAME_LEN, name.len() as u16);
    rec[REC_NAME..REC_NAME + name.len()].copy_from_slice(name.as_bytes());
    wr_u32(rec, REC_DATA_LEN, data.len() as u32);
    rec[REC_DATA..REC_DATA + data.len()].copy_from_slice(data.as_bytes());
    wr_u64(region, OFF_WRITE_SEQ, seq);
    seq
}

/// The producer's side, under the events lock: the queue's dropped count so far.
pub fn ring_set_dropped(region: &mut [u8], dropped: u64) {
    wr_u64(region, OFF_DROPPED, dropped);
}

/// The ring's header, checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingHead {
    pub write_seq: u64,
    pub dropped: u64,
}

/// Check the ring's size, magic and version, and read its counters.
pub fn read_ring_head(region: &[u8]) -> Result<RingHead, ReadError> {
    if region.len() < RING_SIZE {
        return Err(ReadError::Layout(format!(
            "event ring is {} bytes, under {RING_SIZE}",
            region.len()
        )));
    }
    if region[..4] != RING_MAGIC {
        return Err(ReadError::Layout("event ring magic is not UWDE".into()));
    }
    let version = rd_u32(region, OFF_VERSION);
    if version != FORMAT_VERSION {
        return Err(ReadError::Version(version));
    }
    Ok(RingHead {
        write_seq: rd_u64(region, OFF_WRITE_SEQ),
        dropped: rd_u64(region, OFF_DROPPED),
    })
}

/// One event as the helper read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingEvent {
    pub seq: u64,
    pub name: String,
    pub data: String,
    pub ts: u64,
}

/// One drain: `lost` events (`data_lost`, sent first), then `events` in seq order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Drained {
    pub lost: u64,
    pub events: Vec<RingEvent>,
}

fn read_record(region: &[u8], seq: u64) -> Result<RingEvent, ReadError> {
    let rec = &region[slot(seq)..slot(seq) + RECORD_SIZE];
    let layout_err = |why: String| Err(ReadError::Layout(why));
    let stored = rd_u64(rec, REC_SEQ);
    if stored != seq {
        return layout_err(format!("the ring slot for seq {seq} holds seq {stored}"));
    }
    let name_len = rd_u16(rec, REC_NAME_LEN) as usize;
    if name_len > MAX_EVENT_NAME {
        return layout_err(format!(
            "event {seq}: name length {name_len} over {MAX_EVENT_NAME}"
        ));
    }
    let data_len = rd_u32(rec, REC_DATA_LEN) as usize;
    if data_len > MAX_EVENT_DATA {
        return layout_err(format!(
            "event {seq}: data length {data_len} over {MAX_EVENT_DATA}"
        ));
    }
    let Ok(name) = String::from_utf8(rec[REC_NAME..REC_NAME + name_len].to_vec()) else {
        return layout_err(format!("event {seq}: name is not UTF-8"));
    };
    let Ok(data) = String::from_utf8(rec[REC_DATA..REC_DATA + data_len].to_vec()) else {
        return layout_err(format!("event {seq}: data is not UTF-8"));
    };
    Ok(RingEvent {
        seq,
        name,
        data,
        ts: rd_u64(rec, REC_TS),
    })
}

/// The helper's side: where it is in one producer's ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingReader {
    /// The last seq delivered (or the start point).
    after: u64,
    /// The producer's dropped count already reported (or seen at the start).
    dropped: u64,
}

impl RingReader {
    /// Attach to a ring (design flow step 4): check it and pick the start with `start_after`.
    /// Returns the reader and the start (`data_connected.from_seq`). Only `dropped` growth from
    /// here on is lost. A resume point ahead of the ring's write head is a bad layout.
    pub fn start(
        region: &[u8],
        producer_id: u64,
        resume: Option<ResumePoint>,
    ) -> Result<(RingReader, u64), ReadError> {
        let head = read_ring_head(region)?;
        let after = start_after(producer_id, head.write_seq, resume);
        catch_up(after, head.write_seq)?;
        Ok((
            RingReader {
                after,
                dropped: head.dropped,
            },
            after,
        ))
    }

    /// The last seq delivered.
    pub fn after(&self) -> u64 {
        self.after
    }

    /// Everything after the last drain, in seq order, and what was lost before it: records
    /// overwritten before they were read, plus growth in the producer's `dropped`. A seq or a
    /// dropped count that goes backwards is a bad layout.
    pub fn drain(&mut self, region: &[u8]) -> Result<Drained, ReadError> {
        let head = read_ring_head(region)?;
        if head.dropped < self.dropped {
            return Err(ReadError::Layout(format!(
                "the dropped count went backwards: {} after {}",
                head.dropped, self.dropped
            )));
        }
        let c = catch_up(self.after, head.write_seq)?;
        let mut events = Vec::new();
        for seq in c.first..=head.write_seq {
            events.push(read_record(region, seq)?);
        }
        let lost = c.lost + (head.dropped - self.dropped);
        self.after = head.write_seq;
        self.dropped = head.dropped;
        Ok(Drained { lost, events })
    }
}
```

`crates/uw-pipe-proto/src/data.rs` (complete file):

```rust
//! The game-data channel's layouts (design 2b "Messages and layouts"), as byte-slice code with
//! fixed little-endian offsets, so they are unit-tested on Linux like `state`. The producer in the
//! game creates the objects and writes them; the helper only reads them, and checks every read.
//! The Win32 objects that hold these bytes are `uw_game_channel::data`.
//!
//! - `hello`: the 24-byte hello block (magic, format version, producer pid, `producer_id`).
//! - `schema`: the info store's categories of keys, each key with a maximum value length.
//! - `info`: the info store (header, schema, per-key timestamps and value slots).
//! - `ring`: the event ring (header, 30 records).
//! - `resume`: where event delivery starts and resumes, as pure arithmetic on seqs.

mod hello;
mod info;
mod resume;
mod ring;
mod schema;

pub use hello::{HELLO_MAGIC, HELLO_SIZE, Hello, read_hello, write_hello};
pub use info::{
    INFO_HEADER_SIZE, INFO_MAGIC, InfoLayout, InfoReader, InfoValue, info_init, info_set,
};
pub use resume::{BadResumePoint, CatchUp, RING_SLOTS, ResumePoint, catch_up, start_after};
pub use ring::{
    Drained, MAX_EVENT_DATA, MAX_EVENT_NAME, RECORD_SIZE, RING_HEADER_SIZE, RING_MAGIC, RING_SIZE,
    RingEvent, RingHead, RingReader, read_ring_head, ring_init, ring_push, ring_set_dropped,
};
pub use schema::{
    MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, Schema, SchemaCategory, SchemaError, SchemaKey,
};

/// The format version of the hello block, the info store and the event ring.
pub const FORMAT_VERSION: u32 = 1;
/// The channel milestone 1 uses (design 2b "Scope"); the helper's `--data-channel` defaults to
/// it.
pub const DEFAULT_CHANNEL: u32 = 1;

/// Why bytes the helper read from the channel cannot be used (design 2b "Failures"). The helper
/// reports it once per producer, stops reading and waits for `disconnect`; it is not a stop code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A hello block or a header with another format version (`data_version_mismatch`).
    Version(u32),
    /// Anything else that does not fit (`data_bad_layout`), and why.
    Layout(String),
}

impl ReadError {
    /// The JSON `error` code the helper reports.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::Version(_) => "data_version_mismatch",
            ReadError::Layout(_) => "data_bad_layout",
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Version(v) => write!(f, "format version {v}, not {FORMAT_VERSION}"),
            ReadError::Layout(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for ReadError {}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}
fn wr_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
```

- [ ] **Step 4: Run the tests, clippy and rustfmt**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-pipe-proto/src/data.rs crates/uw-pipe-proto/tests/data_ring.rs
```
Expected, all passing: lib unit tests 3, `bells` 3, `data_info` 6, `data_ring` 11, `data_schema` 6, `jsonl` 8, `pipe` 19, `state` 5; both clippies clean; rustfmt prints nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "data: the event ring and the resume arithmetic"
```

- [ ] **Step 6: Check that the gap rule bites, then restore**

In `crates/uw-pipe-proto/src/data/resume.rs`, replace `    let first = next.max(oldest);` with `    let first = next;` (a start the ring has overwritten is read anyway). Run:
```bash
cargo test -p uw-pipe-proto --test data_ring
git checkout -- crates/uw-pipe-proto/src/data/resume.rs
cargo test -p uw-pipe-proto --test data_ring
```
Expected: the first run FAILS three tests, among them `catch_up_gives_the_records_to_deliver_and_the_gap` with `left: Ok(CatchUp { lost: 0, first: 1 })`, `right: Ok(CatchUp { lost: 1, first: 2 })`, and `wraparound_before_a_drain_is_counted_as_lost`; after the restore `11 passed`; `git status` is clean.

---

## Task 4: The producer's outbox: pending info, the event queue and its throttle

Design "Producer API" and "Differences from Spike 3", as pure code so the rules are unit-tested on Linux (Task 5's `Producer` wraps it). `commit` checks every entry, then keeps the newest value per key with its own timestamp until `write_info` runs under the info lock. `push_event` checks the limits and queues into a FIFO of 256; past that the oldest is dropped and counted. `write_events` writes queued events while the one throttle (30 per 200 ms, sliding) admits them, then the dropped count, into the ring's header. Nothing is silently dropped or truncated: errors go to the caller as `ProduceError`.

**Files:**
- Modify: `crates/uw-pipe-proto/src/data.rs`
- Create: `crates/uw-pipe-proto/src/data/outbox.rs`, `crates/uw-pipe-proto/tests/data_outbox.rs`

- [ ] **Step 1: Write the failing tests**

`crates/uw-pipe-proto/tests/data_outbox.rs` (complete file):

```rust
use uw_pipe_proto::data::{
    InfoReader, InfoValue, Outbox, ProduceError, QUEUE_CAP, RING_SIZE, RingReader, Schema,
    SchemaCategory, THROTTLE_MAX, THROTTLE_WINDOW_MS, Throttle, info_init, ring_init,
};

fn outbox() -> (Outbox, Vec<u8>) {
    let s = Schema::new(vec![SchemaCategory::new(
        "test",
        &[("ticks", 4), ("last_seq", 8)],
    )])
    .unwrap();
    let o = Outbox::new(s);
    let mut info = vec![0u8; o.info_layout().size()];
    info_init(&mut info, o.schema(), o.info_layout());
    (o, info)
}

fn ring() -> Vec<u8> {
    let mut r = vec![0u8; RING_SIZE];
    ring_init(&mut r);
    r
}

fn value(key: &str, v: Option<&str>, ts: u64) -> InfoValue {
    InfoValue {
        category: "test".into(),
        key: key.into(),
        value: v.map(str::to_string),
        ts,
    }
}

/// Catches a throttle other than the design's one window (30 events per 200 ms, sliding), or one
/// that admits early.
#[test]
fn the_throttle_admits_30_per_200_ms_sliding() {
    assert_eq!((THROTTLE_MAX, THROTTLE_WINDOW_MS), (30, 200));
    let mut t = Throttle::new(THROTTLE_WINDOW_MS, THROTTLE_MAX);
    for _ in 0..10 {
        assert!(t.admit(1_000));
    }
    for _ in 0..20 {
        assert!(t.admit(1_100));
    }
    assert!(!t.admit(1_100));
    assert!(!t.admit(1_199));
    // The 10 from t=1000 have left the window; the 20 from t=1100 have not.
    for _ in 0..10 {
        assert!(t.admit(1_200));
    }
    assert!(!t.admit(1_250));
    assert!(t.admit(1_300));
}

/// Catches a commit that keeps part of a transaction it refused, or one that accepts an unknown
/// key or a value over its `max_len` (design: errors go to the caller, nothing is truncated).
#[test]
fn a_commit_with_a_bad_entry_is_refused_whole() {
    let (mut o, mut info) = outbox();
    assert_eq!(
        o.commit(
            &[("test", "ticks", Some("1")), ("test", "nope", Some("x"))],
            5
        ),
        Err(ProduceError::UnknownKey {
            category: "test".into(),
            key: "nope".into()
        })
    );
    assert_eq!(
        o.commit(&[("test", "ticks", Some("12345"))], 5),
        Err(ProduceError::ValueTooLong {
            category: "test".into(),
            key: "ticks".into(),
            len: 5,
            max_len: 4
        })
    );
    assert_eq!(
        o.commit(&[("other", "ticks", None)], 5),
        Err(ProduceError::UnknownKey {
            category: "other".into(),
            key: "ticks".into()
        })
    );
    assert!(!o.has_pending_info());
    assert!(!o.write_info(&mut info), "nothing was kept");
    assert_eq!(
        o.commit(&[("test", "ticks", Some("1234"))], 5),
        Ok(()),
        "max_len fits"
    );
}

/// Catches pending keys sharing one timestamp after a busy info lock (Spike 3's merge, which the
/// design replaces: "each pending key keeps its own"), or an older pending value winning.
#[test]
fn pending_keys_keep_their_own_timestamps_and_the_newest_value() {
    let (mut o, mut info) = outbox();
    // Two passes while the helper held the info lock: nothing was written in between.
    o.commit(&[("test", "ticks", Some("1"))], 10).unwrap();
    o.commit(
        &[("test", "last_seq", None), ("test", "ticks", Some("2"))],
        20,
    )
    .unwrap();
    o.commit(&[("test", "last_seq", Some("7"))], 30).unwrap();
    assert!(o.has_pending_info());
    assert!(o.write_info(&mut info));
    assert!(!o.has_pending_info());
    let mut r = InfoReader::attach(&info).unwrap();
    assert_eq!(
        r.snapshot(&info).unwrap(),
        [
            value("ticks", Some("2"), 20),
            value("last_seq", Some("7"), 30)
        ]
    );
    // An unchanged value changes nothing, so no bell.
    o.commit(&[("test", "ticks", Some("2"))], 40).unwrap();
    assert!(!o.write_info(&mut info));
    assert_eq!(r.changes(&info).unwrap(), []);
}

/// Catches an event over the design's limits being queued (or truncated) instead of refused.
#[test]
fn an_event_over_its_limits_is_refused() {
    let (mut o, _) = outbox();
    assert_eq!(
        o.push_event(&"n".repeat(256), "", 1),
        Err(ProduceError::NameTooLong(256))
    );
    assert_eq!(
        o.push_event("e", &"d".repeat(63 * 1024 + 1), 1),
        Err(ProduceError::DataTooLong(64_513))
    );
    assert_eq!(o.queued(), 0);
    assert!(
        o.push_event(&"n".repeat(255), &"d".repeat(63 * 1024), 1)
            .is_ok()
    );
}

/// Catches queue overflow that is not counted in `dropped` (design: "oldest dropped and
/// counted"), a dropped count that never reaches the ring's header, events over the throttle
/// being lost instead of waiting in the queue, or events written out of order.
#[test]
fn queue_overflow_is_counted_and_the_rest_wait_for_the_throttle() {
    let (mut o, _) = outbox();
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 1, None).unwrap();
    for i in 0..300u64 {
        o.push_event("burst", &i.to_string(), i).unwrap();
    }
    assert_eq!(QUEUE_CAP, 256);
    assert_eq!((o.queued(), o.dropped()), (256, 44));
    assert!(o.has_pending_events());

    assert!(o.write_events(&mut r, 1_000));
    assert_eq!(o.queued(), 226);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 44, "the dropped count reached the ring");
    let data: Vec<String> = d.events.iter().map(|e| e.data.clone()).collect();
    let want: Vec<String> = (44..74).map(|i: u64| i.to_string()).collect();
    assert_eq!(data, want, "the oldest went, the rest are in order");
    assert_eq!(
        d.events[0].ts, 44,
        "each event keeps the time it was pushed"
    );

    assert!(
        !o.write_events(&mut r, 1_100),
        "the throttle's budget is spent"
    );
    assert_eq!(o.queued(), 226);
    assert!(o.write_events(&mut r, 1_200));
    assert_eq!(o.queued(), 196);
    let d = reader.drain(&r).unwrap();
    assert_eq!((d.lost, d.events.len()), (0, 30));
    assert_eq!(d.events[0].data, "74");

    let mut now = 1_400;
    while o.queued() > 0 {
        o.write_events(&mut r, now);
        reader.drain(&r).unwrap();
        now += 200;
    }
    assert!(!o.has_pending_events());
    assert!(!o.write_events(&mut r, now));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test data_outbox`
Expected: does not compile: `` error[E0432]: unresolved imports `uw_pipe_proto::data::Outbox`, `uw_pipe_proto::data::ProduceError`, … `` (all six names).

- [ ] **Step 3: Add the outbox**

`crates/uw-pipe-proto/src/data/outbox.rs` (complete file):

```rust
//! The producer's pending work (design 2b "Producer API"), without the Win32 objects, so its
//! rules are unit-tested on Linux. `uw_game_channel::data::Producer` wraps it.
//!
//! - `commit` checks every entry first (an unknown key or a value over its `max_len` is an error,
//!   and then nothing is kept), then keeps the newest value per key, each with its own timestamp,
//!   until `write_info` runs under the info lock.
//! - `push_event` checks the name and the data, then queues the event in a FIFO of `QUEUE_CAP`;
//!   past that the oldest is dropped and counted.
//! - `write_events` writes queued events into the ring under the events lock, as many as one
//!   throttle of `THROTTLE_MAX` per `THROTTLE_WINDOW_MS` admits (the rest wait in the queue), and
//!   the dropped count with them. A busy lock spends nothing: the throttle is asked only once the
//!   lock is held.

use std::collections::{BTreeMap, VecDeque};

use super::{
    InfoLayout, MAX_EVENT_DATA, MAX_EVENT_NAME, Schema, info_set, ring_push, ring_set_dropped,
};

pub const QUEUE_CAP: usize = 256;
pub const THROTTLE_MAX: usize = 30;
pub const THROTTLE_WINDOW_MS: u64 = 200;

/// Why the producer refused a call. Nothing is silently dropped or truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProduceError {
    UnknownKey {
        category: String,
        key: String,
    },
    ValueTooLong {
        category: String,
        key: String,
        len: usize,
        max_len: u32,
    },
    NameTooLong(usize),
    DataTooLong(usize),
}

impl std::fmt::Display for ProduceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProduceError::UnknownKey { category, key } => {
                write!(f, "{category}.{key} is not in the schema")
            }
            ProduceError::ValueTooLong {
                category,
                key,
                len,
                max_len,
            } => {
                write!(
                    f,
                    "{category}.{key}: a value of {len} bytes, over its max_len {max_len}"
                )
            }
            ProduceError::NameTooLong(n) => {
                write!(f, "an event name of {n} bytes, over {MAX_EVENT_NAME}")
            }
            ProduceError::DataTooLong(n) => {
                write!(f, "event data of {n} bytes, over {MAX_EVENT_DATA}")
            }
        }
    }
}
impl std::error::Error for ProduceError {}

/// A sliding-window throttle: at most `max` admitted in any `window_ms`.
#[derive(Debug, Clone)]
pub struct Throttle {
    window_ms: u64,
    max: usize,
    admitted: VecDeque<u64>,
}

impl Throttle {
    pub fn new(window_ms: u64, max: usize) -> Throttle {
        Throttle {
            window_ms,
            max,
            admitted: VecDeque::with_capacity(max),
        }
    }

    /// Admit one more at `now` (ms) if fewer than `max` were admitted in the last `window_ms`.
    pub fn admit(&mut self, now: u64) -> bool {
        while self
            .admitted
            .front()
            .is_some_and(|&t| now.saturating_sub(t) >= self.window_ms)
        {
            self.admitted.pop_front();
        }
        if self.admitted.len() >= self.max {
            return false;
        }
        self.admitted.push_back(now);
        true
    }
}

#[derive(Debug, Clone)]
struct Queued {
    name: String,
    data: String,
    ts: u64,
}

/// The producer's pending info and queued events.
#[derive(Debug, Clone)]
pub struct Outbox {
    schema: Schema,
    layout: InfoLayout,
    /// Key index -> the newest value committed and its timestamp.
    pending: BTreeMap<usize, (Option<String>, u64)>,
    queue: VecDeque<Queued>,
    dropped: u64,
    /// The dropped count last written into the ring.
    ring_dropped: u64,
    throttle: Throttle,
}

impl Outbox {
    /// The schema is parsed once, here (the `Producer` is created with it).
    pub fn new(schema: Schema) -> Outbox {
        Outbox {
            layout: InfoLayout::of(&schema),
            schema,
            pending: BTreeMap::new(),
            queue: VecDeque::new(),
            dropped: 0,
            ring_dropped: 0,
            throttle: Throttle::new(THROTTLE_WINDOW_MS, THROTTLE_MAX),
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn info_layout(&self) -> &InfoLayout {
        &self.layout
    }

    /// One info transaction at time `ts`: `(category, key, value)` entries, `None` for null.
    pub fn commit(
        &mut self,
        entries: &[(&str, &str, Option<&str>)],
        ts: u64,
    ) -> Result<(), ProduceError> {
        let mut checked = Vec::with_capacity(entries.len());
        for &(category, key, value) in entries {
            let Some(i) = self.schema.key_index(category, key) else {
                return Err(ProduceError::UnknownKey {
                    category: category.into(),
                    key: key.into(),
                });
            };
            let max_len = self.layout.max_len(i);
            if let Some(v) = value
                && v.len() > max_len as usize
            {
                return Err(ProduceError::ValueTooLong {
                    category: category.into(),
                    key: key.into(),
                    len: v.len(),
                    max_len,
                });
            }
            checked.push((i, value));
        }
        for (i, value) in checked {
            self.pending.insert(i, (value.map(str::to_string), ts));
        }
        Ok(())
    }

    /// Whether `write_info` has anything to write.
    pub fn has_pending_info(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Under the info lock: write every pending value with its own timestamp. Returns whether
    /// any key changed (then the producer rings `info-bell` once).
    pub fn write_info(&mut self, region: &mut [u8]) -> bool {
        let mut changed = false;
        for (i, (value, ts)) in std::mem::take(&mut self.pending) {
            changed |= info_set(region, &self.layout, i, value.as_deref(), ts);
        }
        changed
    }

    /// Queue an event pushed at time `ts`. A full queue drops its oldest event and counts it.
    pub fn push_event(&mut self, name: &str, data: &str, ts: u64) -> Result<(), ProduceError> {
        if name.len() > MAX_EVENT_NAME {
            return Err(ProduceError::NameTooLong(name.len()));
        }
        if data.len() > MAX_EVENT_DATA {
            return Err(ProduceError::DataTooLong(data.len()));
        }
        if self.queue.len() == QUEUE_CAP {
            self.queue.pop_front();
            self.dropped += 1;
        }
        self.queue.push_back(Queued {
            name: name.into(),
            data: data.into(),
            ts,
        });
        Ok(())
    }

    /// Events waiting for the ring.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Events the queue dropped so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether `write_events` has anything to write (events, or a new dropped count).
    pub fn has_pending_events(&self) -> bool {
        !self.queue.is_empty() || self.dropped != self.ring_dropped
    }

    /// Under the events lock, at time `now`: write queued events in order while the throttle
    /// admits them, then the dropped count. Returns whether the ring changed (then the producer
    /// rings `events-bell` once).
    pub fn write_events(&mut self, region: &mut [u8], now: u64) -> bool {
        let mut changed = false;
        while !self.queue.is_empty() && self.throttle.admit(now) {
            if let Some(e) = self.queue.pop_front() {
                ring_push(region, &e.name, &e.data, e.ts);
                changed = true;
            }
        }
        if self.dropped != self.ring_dropped {
            ring_set_dropped(region, self.dropped);
            self.ring_dropped = self.dropped;
            changed = true;
        }
        changed
    }
}
```

`crates/uw-pipe-proto/src/data.rs` (complete file):

```rust
//! The game-data channel's layouts (design 2b "Messages and layouts"), as byte-slice code with
//! fixed little-endian offsets, so they are unit-tested on Linux like `state`. The producer in the
//! game creates the objects and writes them; the helper only reads them, and checks every read.
//! The Win32 objects that hold these bytes are `uw_game_channel::data`.
//!
//! - `hello`: the 24-byte hello block (magic, format version, producer pid, `producer_id`).
//! - `schema`: the info store's categories of keys, each key with a maximum value length.
//! - `info`: the info store (header, schema, per-key timestamps and value slots).
//! - `ring`: the event ring (header, 30 records).
//! - `resume`: where event delivery starts and resumes, as pure arithmetic on seqs.
//! - `outbox`: the producer's pending info and event queue, with its throttle.

mod hello;
mod info;
mod outbox;
mod resume;
mod ring;
mod schema;

pub use hello::{HELLO_MAGIC, HELLO_SIZE, Hello, read_hello, write_hello};
pub use info::{
    INFO_HEADER_SIZE, INFO_MAGIC, InfoLayout, InfoReader, InfoValue, info_init, info_set,
};
pub use outbox::{Outbox, ProduceError, QUEUE_CAP, THROTTLE_MAX, THROTTLE_WINDOW_MS, Throttle};
pub use resume::{BadResumePoint, CatchUp, RING_SLOTS, ResumePoint, catch_up, start_after};
pub use ring::{
    Drained, MAX_EVENT_DATA, MAX_EVENT_NAME, RECORD_SIZE, RING_HEADER_SIZE, RING_MAGIC, RING_SIZE,
    RingEvent, RingHead, RingReader, read_ring_head, ring_init, ring_push, ring_set_dropped,
};
pub use schema::{
    MAX_KEYS, MAX_NAME_LEN, MAX_VALUE_LEN, Schema, SchemaCategory, SchemaError, SchemaKey,
};

/// The format version of the hello block, the info store and the event ring.
pub const FORMAT_VERSION: u32 = 1;
/// The channel milestone 1 uses (design 2b "Scope"); the helper's `--data-channel` defaults to
/// it.
pub const DEFAULT_CHANNEL: u32 = 1;

/// Why bytes the helper read from the channel cannot be used (design 2b "Failures"). The helper
/// reports it once per producer, stops reading and waits for `disconnect`; it is not a stop code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// A hello block or a header with another format version (`data_version_mismatch`).
    Version(u32),
    /// Anything else that does not fit (`data_bad_layout`), and why.
    Layout(String),
}

impl ReadError {
    /// The JSON `error` code the helper reports.
    pub fn code(&self) -> &'static str {
        match self {
            ReadError::Version(_) => "data_version_mismatch",
            ReadError::Layout(_) => "data_bad_layout",
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Version(v) => write!(f, "format version {v}, not {FORMAT_VERSION}"),
            ReadError::Layout(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for ReadError {}

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn rd_u64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}
fn wr_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
```

- [ ] **Step 4: Run the tests, clippy and rustfmt**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy -p uw-pipe-proto --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-pipe-proto --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-pipe-proto/src/data.rs crates/uw-pipe-proto/tests/data_outbox.rs
```
Expected, all passing: lib unit tests 3, `bells` 3, `data_info` 6, `data_outbox` 5, `data_ring` 11, `data_schema` 6, `jsonl` 8, `pipe` 19, `state` 5; both clippies clean; rustfmt prints nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "data: the producer's outbox (pending info, the event queue and its throttle)"
```

- [ ] **Step 6: Check that the drop count bites, then restore**

In `crates/uw-pipe-proto/src/data/outbox.rs`, replace `        if self.queue.len() == QUEUE_CAP {` with `        if self.queue.len() == usize::MAX {` (an unbounded queue that never drops). Run:
```bash
cargo test -p uw-pipe-proto --test data_outbox
git checkout -- crates/uw-pipe-proto/src/data/outbox.rs
cargo test -p uw-pipe-proto --test data_outbox
```
Expected: the first run FAILS `queue_overflow_is_counted_and_the_rest_wait_for_the_throttle` with `left: (300, 0)`, `right: (256, 44)`; after the restore `5 passed`; `git status` is clean.

---

## Task 5: Gate: the producer and the consumer between two Wine processes

Design "Components and flow" steps 1-6 and "Testing" 2 (the Wine gate). `uw-game-channel::data` builds the Win32 objects around Tasks 1-4's layouts:
- `Producer::create(channel, schema)` creates every object in the design's order (hello block; info store with its schema written in; event ring; both locks; both bells; `disconnect`, then `connect`), writes the hello block last (`producer_id` = `GetTickCount64()`), and sets `connect`. Any object that already exists fails it with `Exists`. `commit`, `push_event` and `flush` take `&mut self` and try the locks with 0 ms only; `ts` is `QueryPerformanceCounter` in ms. Dropping it sets `disconnect` and resets `connect`.
- `Consumer::open(pid, channel)` needs `connect` set, opens every other object and maps the three regions read-only (`FILE_MAP_READ`). `attach` checks the hello block, takes the snapshot under one info lock and picks the start under the events lock; `changes` and `drain` re-check the headers on every read; locks wait at most 100 ms (`LOCK_TIMEOUT_MS`) and `WAIT_ABANDONED` counts as acquired. `wait` is `[disconnect, info-bell, events-bell]`.

Two command-driven probes run them in two Wine processes of one prefix (a second consumer probe joins at the end), and the test checks every answer: the snapshot (null included), the bells `create()` rang taken first so that a delta must ring its own, a delta, events in order, a first attach at the write head, a resume (including one older than the oldest record), ring wraparound giving `lost`, a busy lock given up after about 100 ms, an abandoned lock with the header intact (read) and broken (`data_bad_layout`), `disconnect` winning a tie, a channel whose `connect` is reset refused (`not ready`) although its objects still exist, and `Exists` until the consumer lets go. Spike 3 proved these objects under Wine. **If the gate fails, stop and report to the owner.**

**Files:**
- Modify: `crates/uw-game-channel/Cargo.toml`, `crates/uw-game-channel/src/lib.rs`, `crates/uw-game-channel/src/objects.rs`
- Create: `crates/uw-game-channel/src/data.rs`, `crates/uw-game-channel/examples/data_producer_probe.rs`, `crates/uw-game-channel/examples/data_consumer_probe.rs`, `crates/uw-game-channel/tests/wine_data.rs`

- [ ] **Step 1: Write the probes and the gate test**

`crates/uw-game-channel/examples/data_producer_probe.rs` (complete file):

```rust
//! Data gate probe, producer side (design 2b "Testing" 2). Run under Wine next to
//! `data_consumer_probe`, in the same prefix. It creates channel 1 of its own process
//! (`Producer::create`) with the schema `probe` {`a`: 16, `b`: 16}, commits `a = "1"` and
//! `b = null`, pushes three events (`first`/`a`, `second`/`b`, `third`/`c`), flushes, and prints
//! `ready pid=<windows pid>`. Then it runs one command per stdin line and answers each with one
//! line, `ok` unless said otherwise:
//! - `delta`: commit `a = "2"`, push `fourth`/`d`, flush;
//! - `burst <n>`: push n events `burst` (data `0`, `1`, …), then flush until all are written (the
//!   throttle writes at most 30 per 200 ms);
//! - `hold-info <ms>`: a thread takes the info lock and holds it for ms (`ok` once it has it);
//! - `abandon-events`: a thread takes the events lock and ends without releasing it;
//! - `abandon-info`: a thread takes the info lock, breaks the info store's magic, and ends
//!   without releasing it;
//! - `drop`: drop the producer (it sets `disconnect`);
//! - `recreate`: `Producer::create` again: `ok`, or `exists` while a consumer still holds the
//!   old objects.
//!
//! It exits when stdin closes.

#[cfg(not(windows))]
fn main() {
    eprintln!("data_producer_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::{BufRead, Write};
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use uw_game_channel::data::{CreateError, Producer};
    use uw_pipe_proto::data::{Schema, SchemaCategory};
    use uw_pipe_proto::names::{DataObject, data_name};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Memory::{
        FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, INFINITE, MUTEX_MODIFY_STATE, OpenMutexW, ReleaseMutex,
        SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
    };

    const CHANNEL: u32 = 1;

    fn say(s: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    }

    fn schema() -> Schema {
        Schema::new(vec![SchemaCategory::new("probe", &[("a", 16), ("b", 16)])])
            .expect("the probe schema fits")
    }

    fn name(object: DataObject) -> Vec<u16> {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        data_name(pid, CHANNEL, object)
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    /// Open one of our channel's locks by name, apart from the producer's own handle.
    fn open_lock(object: DataObject) -> HANDLE {
        let n = name(object);
        // SAFETY: opens an existing named mutex to wait on it and release it.
        let h = unsafe {
            OpenMutexW(
                SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                0,
                n.as_ptr(),
            )
        };
        assert!(!h.is_null(), "OpenMutexW failed");
        h
    }

    /// On a thread of its own: take `object`'s lock, run `then` while holding it, and either
    /// release it after `hold_ms` or end the thread without releasing it (`hold_ms = None`), which
    /// abandons it. Answers `ok` once the lock is held.
    fn lock_on_thread(object: DataObject, hold_ms: Option<u64>, then: fn()) {
        let (tx, rx) = channel();
        let t = std::thread::spawn(move || {
            let h = open_lock(object);
            // SAFETY: waits on the mutex handle opened just above.
            unsafe { WaitForSingleObject(h, INFINITE) };
            then();
            let _ = tx.send(());
            if let Some(ms) = hold_ms {
                std::thread::sleep(Duration::from_millis(ms));
                // SAFETY: releases the mutex this thread acquired just above.
                unsafe { ReleaseMutex(h) };
            }
            // SAFETY: closes the handle opened above. A mutex still owned is abandoned when this
            // thread ends.
            unsafe { CloseHandle(h) };
        });
        let _ = rx.recv();
        if hold_ms.is_none() {
            let _ = t.join();
        }
        say("ok");
    }

    fn break_info_magic() {
        let n = name(DataObject::Info);
        // SAFETY: opens our own info mapping for writing, writes one byte of its magic (both
        // handles checked first), and unmaps and closes it again.
        unsafe {
            let h = OpenFileMappingW(FILE_MAP_WRITE, 0, n.as_ptr());
            assert!(!h.is_null(), "OpenFileMappingW failed");
            let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 0);
            assert!(!view.Value.is_null(), "MapViewOfFile failed");
            *view.Value.cast::<u8>() = b'X';
            UnmapViewOfFile(view);
            CloseHandle(h);
        }
    }

    fn create() -> Result<Producer, CreateError> {
        let mut p = Producer::create(CHANNEL, schema())?;
        p.commit(&[("probe", "a", Some("1")), ("probe", "b", None)])
            .expect("commit");
        for (name, data) in [("first", "a"), ("second", "b"), ("third", "c")] {
            p.push_event(name, data).expect("push_event");
        }
        p.flush();
        Ok(p)
    }

    pub fn main() -> i32 {
        let mut producer = match create() {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[data_producer_probe] create: {e}");
                return 3;
            }
        };
        // SAFETY: no arguments.
        say(&format!("ready pid={}", unsafe { GetCurrentProcessId() }));
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let mut words = line.split_whitespace();
            match (words.next().unwrap_or(""), producer.as_mut()) {
                ("delta", Some(p)) => {
                    p.commit(&[("probe", "a", Some("2"))]).expect("commit");
                    p.push_event("fourth", "d").expect("push_event");
                    p.flush();
                    say("ok");
                }
                ("burst", Some(p)) => {
                    let n: u64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                    for i in 0..n {
                        p.push_event("burst", &i.to_string()).expect("push_event");
                    }
                    while p.queued() > 0 {
                        p.flush();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    say("ok");
                }
                ("hold-info", Some(_)) => {
                    let ms = words.next().and_then(|w| w.parse().ok()).unwrap_or(1000);
                    lock_on_thread(DataObject::InfoLock, Some(ms), || {});
                }
                ("abandon-events", Some(_)) => lock_on_thread(DataObject::EventsLock, None, || {}),
                ("abandon-info", Some(_)) => {
                    lock_on_thread(DataObject::InfoLock, None, break_info_magic)
                }
                ("drop", Some(_)) => {
                    producer = None;
                    say("ok");
                }
                ("recreate", None) => match create() {
                    Ok(p) => {
                        producer = Some(p);
                        say("ok");
                    }
                    Err(CreateError::Exists(_)) => say("exists"),
                    Err(e) => say(&format!("error {e}")),
                },
                (other, _) => say(&format!("error unknown command {other:?}")),
            }
        }
        0
    }
}
```

`crates/uw-game-channel/examples/data_consumer_probe.rs` (complete file):

```rust
//! Data gate probe, consumer side (design 2b "Testing" 2). `data_consumer_probe <producer windows
//! pid>`, run under Wine in the prefix where `data_producer_probe` runs. It runs one command per
//! stdin line, as the helper's data thread would, and answers each with one line:
//! - `open`: `Consumer::open` every 100 ms until it opens (`opened`; 30 s at most);
//! - `open-once`: `Consumer::open` once: `opened`, or `not ready` (`connect` missing or not set);
//! - `attach [<producer_id>:<seq>]`: `attached producer_id=<id> from_seq=<n> schema=<c.k/max,…>
//!   snapshot=<c.k=value,…>` (null values print as `null`);
//! - `wait`: `wake disconnect|info|events|timeout` (a 5 s wait);
//! - `changes`: `changes <c.k=value> …`;
//! - `drain`: `drain lost=<n> <seq>:<name>:<data> …`;
//! - `close`: drop the consumer (`closed`).
//!
//! A read that fails answers `busy <ms>` (the lock was not acquired, after that many ms) or
//! `error <code> <reason>`. It exits when stdin closes.

#[cfg(not(windows))]
fn main() {
    eprintln!("data_consumer_probe runs inside Wine; build it for x86_64-pc-windows-gnu");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::main());
}

#[cfg(windows)]
mod win {
    use std::io::{BufRead, Write};
    use std::time::{Duration, Instant};

    use uw_game_channel::data::{ConsumeError, Consumer, OpenError, Wake};
    use uw_pipe_proto::data::{InfoValue, ResumePoint};

    const CHANNEL: u32 = 1;

    fn say(s: &str) {
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    }

    fn values(vs: &[InfoValue]) -> Vec<String> {
        vs.iter()
            .map(|v| {
                format!(
                    "{}.{}={}",
                    v.category,
                    v.key,
                    v.value.as_deref().unwrap_or("null")
                )
            })
            .collect()
    }

    fn failed(e: ConsumeError, started: Instant) -> String {
        match e {
            ConsumeError::Busy => format!("busy {}", started.elapsed().as_millis()),
            ConsumeError::Read(e) => format!("error {} {e}", e.code()),
        }
    }

    fn command(line: &str, pid: u32, consumer: &mut Option<Consumer>) -> String {
        let mut words = line.split_whitespace();
        let word = words.next().unwrap_or("");
        if word == "open" {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(30) {
                if let Ok(c) = Consumer::open(pid, CHANNEL) {
                    *consumer = Some(c);
                    return "opened".into();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return "error open timed out".into();
        }
        if word == "open-once" {
            return match Consumer::open(pid, CHANNEL) {
                Ok(c) => {
                    *consumer = Some(c);
                    "opened".into()
                }
                Err(OpenError::NotReady) => "not ready".into(),
                Err(e) => format!("error {e}"),
            };
        }
        let Some(c) = consumer.as_mut() else {
            return "error not open".into();
        };
        let started = Instant::now();
        match word {
            "attach" => {
                let resume = words.next().and_then(|w| w.parse::<ResumePoint>().ok());
                match c.attach(resume) {
                    Ok(a) => {
                        let schema: Vec<String> = a
                            .schema
                            .keys()
                            .map(|(c, k, max)| format!("{c}.{k}/{max}"))
                            .collect();
                        format!(
                            "attached producer_id={} from_seq={} schema={} snapshot={}",
                            a.producer_id,
                            a.from_seq,
                            schema.join(","),
                            values(&a.snapshot).join(",")
                        )
                    }
                    Err(e) => failed(e, started),
                }
            }
            "wait" => match c.wait(5000) {
                Wake::Disconnect => "wake disconnect".into(),
                Wake::Info => "wake info".into(),
                Wake::Events => "wake events".into(),
                Wake::Timeout => "wake timeout".into(),
            },
            "changes" => match c.changes() {
                Ok(vs) => ["changes".to_string()]
                    .into_iter()
                    .chain(values(&vs))
                    .collect::<Vec<_>>()
                    .join(" "),
                Err(e) => failed(e, started),
            },
            "drain" => match c.drain() {
                Ok(d) => {
                    let mut out = format!("drain lost={}", d.lost);
                    for e in &d.events {
                        out.push_str(&format!(" {}:{}:{}", e.seq, e.name, e.data));
                    }
                    out
                }
                Err(e) => failed(e, started),
            },
            "close" => {
                *consumer = None;
                "closed".into()
            }
            other => format!("error unknown command {other:?}"),
        }
    }

    pub fn main() -> i32 {
        let Some(pid) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
            eprintln!("usage: data_consumer_probe <producer windows pid>");
            return 2;
        };
        let mut consumer = None;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            say(&command(&line, pid, &mut consumer));
        }
        0
    }
}
```

`crates/uw-game-channel/tests/wine_data.rs` (complete file):

```rust
#![cfg(unix)]
//! The plan 2b gate under plain Wine (system `/usr/bin/wine`, not Proton): two Wine processes in
//! one scratch prefix share a data channel. `data_producer_probe.exe` creates it as the in-game
//! producer will; `data_consumer_probe.exe` reads it as the helper's data thread will. The test
//! drives both, one command at a time, and checks every answer: the snapshot, deltas, events in
//! order, a resume, ring wraparound giving a loss, a busy lock, an abandoned lock with and without
//! a broken header, `disconnect` winning a tie, a channel refused once `connect` is reset, and
//! `Exists` until the consumer lets go.
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
/// included) or a delta not seen, a first attach replaying the ring instead of starting at the
/// write head, events out of order or twice, a resume that misses or repeats events, ring
/// wraparound not counted as lost, a lock wait longer than 100 ms, an abandoned lock treated as
/// busy or used without re-checking the header, `disconnect` losing a tie to a stale bell, a
/// channel opened although `connect` is not set (a producer still creating it, or one that went
/// away), or a producer re-created while the consumer still holds the old objects.
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
    let pid = ready
        .strip_prefix("ready pid=")
        .unwrap_or_else(|| panic!("producer: {ready:?}"))
        .to_string();
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
    assert_eq!(producer.ask("recreate"), "ok");
    assert_eq!(late.ask("open-once"), "opened");
}
```

- [ ] **Step 2: Build them to see them fail**

Run: `cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples`
Expected: neither data probe compiles: `` error[E0432]: unresolved import `uw_game_channel::data` `` (`` could not find `data` in `uw_game_channel` ``), with follow-on errors in the consumer probe. 2a's two probes still build.

- [ ] **Step 3: Share 2a's Win32 helpers**

In `crates/uw-game-channel/src/objects.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
```

with:

```rust
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
```

2. Replace:

```rust
pub const LOCK_TIMEOUT_MS: u32 = 100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLastError() }
```

with:

```rust
pub const LOCK_TIMEOUT_MS: u32 = 100;

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetLastError() }
```

3. Replace:

```rust
}

/// An owned kernel handle, closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
```

with:

```rust
}

/// An owned kernel handle, closed on drop. `data` uses it too.
pub(crate) struct Handle(pub(crate) HANDLE);

impl Drop for Handle {
```

4. Replace:

```rust

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
```

with:

```rust

/// A mapped view of a whole file mapping, unmapped on drop. `len` is the mapped region from
/// `VirtualQuery` (whole pages, so at least the object's size). `data` uses it too.
pub(crate) struct View {
    pub(crate) ptr: *mut u8,
    pub(crate) len: usize,
}

impl View {
    fn map(mapping: &Handle, name: &str) -> Result<View, ObjectsError> {
        View::map_as(mapping, FILE_MAP_READ | FILE_MAP_WRITE, name)
    }

    /// Map the whole object with `access` (`FILE_MAP_READ` alone gives a read-only view).
    pub(crate) fn map_as(mapping: &Handle, access: FILE_MAP, name: &str) -> Result<View, ObjectsError> {
        // SAFETY: maps the whole object with `access`; `VirtualQuery` only describes our own view.
        unsafe {
            let addr = MapViewOfFile(mapping.0, access, 0, 0, 0);
            if addr.Value.is_null() {
                return Err(win32("MapViewOfFile", name));
```

- [ ] **Step 4: Add the producer and the consumer**

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
# opens them (`Objects::open`). `start` runs the in-game worker (overlay pipe, bells, ping). An
# overlay module links it. `data` is the game-data channel (design 2b): `Producer` in the game,
# `Consumer` in the helper. Windows only; on Linux the crate only re-exports the layouts' types.
# The examples are the gate probes run by `tests/wine_objects.rs` (2a) and `tests/wine_data.rs`
# (2b).
[lib]
name = "uw_game_channel"

[dependencies]
uw-pipe-proto = { path = "../uw-pipe-proto" }

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
    "Win32_System_Performance",
    "Win32_System_Pipes",
    "Win32_System_SystemInformation",
    "Win32_System_Threading",
] }
```

`crates/uw-game-channel/src/lib.rs` (complete file):

```rust
//! The control channel between the helper and the in-game overlay (design 2a). `objects` holds
//! the per-game named objects: two auto-reset doorbell events, the 16-byte mask region, the state
//! block and its lock. The helper creates them before its pipes; the game only opens them, after
//! its `overlay_hello`. `start` runs the in-game worker an overlay module uses for all of it
//! (`worker`). `data` is the game-data channel (design 2b): the in-game `Producer` creates its
//! objects, and the helper's `Consumer` reads them. Windows only.

// The worker relies on `catch_unwind` to survive a panic in the overlay's callback; under
// `panic = "abort"` a panic would kill the game instead.
#[cfg(panic = "abort")]
compile_error!("uw-game-channel needs panic = \"unwind\"");

pub use uw_pipe_proto::bells::{to_game, to_host};
pub use uw_pipe_proto::state::StateV1;

#[cfg(windows)]
pub mod data;
#[cfg(windows)]
pub mod objects;
#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use worker::{start, Link};
```

`crates/uw-game-channel/src/data.rs` (complete file):

```rust
//! The game-data channel's named objects (design 2b "Components and flow"), for channel
//! `<channel>` of the game `<pid>` (names from `uw_pipe_proto::names::data_name`): three
//! paging-file mappings (`hello`, `info`, `events`), a lock and a bell for each region, and the
//! `connect`/`disconnect` manual-reset events. The layouts are `uw_pipe_proto::data`.
//!
//! - `Producer`, in the game, creates every object, owns the schema and writes info and events
//!   from one thread of its own (`&mut self`). It never waits on a shared lock: it tries with
//!   0 ms, and a busy lock leaves the work pending for the next call.
//! - `Consumer`, the helper's data thread, opens the objects, maps both regions read-only, waits
//!   at most `LOCK_TIMEOUT_MS` for a lock and checks every read. A lock whose owner died holding
//!   it (`WAIT_ABANDONED`) counts as acquired: every read re-checks the headers anyway.

use uw_pipe_proto::data::{
    Drained, HELLO_SIZE, InfoReader, InfoValue, Outbox, ProduceError, RING_SIZE, ReadError,
    ResumePoint, RingReader, Schema, info_init, read_hello, ring_init, write_hello,
};
use uw_pipe_proto::names::{DataObject, data_name};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, OpenFileMappingW, PAGE_READWRITE,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcessId, MUTEX_MODIFY_STATE, OpenEventW, OpenMutexW,
    ReleaseMutex, ResetEvent, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, Sleep, WaitForMultipleObjects,
    WaitForSingleObject,
};

use crate::objects::{Handle, LOCK_TIMEOUT_MS, ObjectsError, View, last_error, wide};

/// A Win32 call that failed: which call on which object, and its `GetLastError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win32Error {
    pub what: String,
    pub code: u32,
}

impl std::fmt::Display for Win32Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: error {}", self.what, self.code)
    }
}

fn win32(call: &str, name: &str) -> Win32Error {
    Win32Error {
        what: format!("{call} {name}"),
        code: last_error(),
    }
}

impl From<ObjectsError> for Win32Error {
    fn from(e: ObjectsError) -> Win32Error {
        match e {
            ObjectsError::Win32 { what, code } => Win32Error { what, code },
            other => Win32Error {
                what: other.to_string(),
                code: 0,
            },
        }
    }
}

/// Why `Producer::create` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// An object of this channel already exists: another producer's, or an earlier one's that a
    /// helper still holds because it has not seen `disconnect` yet (design "Risks").
    Exists(String),
    Win32(Win32Error),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::Exists(name) => write!(f, "{name} already exists"),
            CreateError::Win32(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for CreateError {}

impl From<Win32Error> for CreateError {
    fn from(e: Win32Error) -> CreateError {
        CreateError::Win32(e)
    }
}

/// A mapping and the whole view of it.
struct Mapping {
    _handle: Handle,
    view: View,
}

impl Mapping {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the view is mapped for `len` bytes while `self` lives. The producer writes the
        // hello block before it sets `connect`, and both regions only under their locks, which
        // every reader of these bytes holds.
        unsafe { std::slice::from_raw_parts(self.view.ptr, self.view.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, on a read/write view; `&mut self` makes this the only borrow in this
        // process, and the caller holds the region's lock (or no one else can open it yet).
        unsafe { std::slice::from_raw_parts_mut(self.view.ptr, self.view.len) }
    }
}

/// A lock held until drop.
struct Locked<'a>(&'a Handle);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: releases the mutex this thread acquired for this guard.
        unsafe { ReleaseMutex(self.0.0) };
    }
}

/// Take `lock` within `timeout_ms`. An abandoned lock counts as acquired (design "Failures"):
/// its owner died holding it, and the reads that follow re-check the headers.
fn lock(mutex: &Handle, timeout_ms: u32) -> Option<Locked<'_>> {
    // SAFETY: waits on our mutex handle.
    let r = unsafe { WaitForSingleObject(mutex.0, timeout_ms) };
    (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(Locked(mutex))
}

/// The `GetLastError` right after a `Create*W` that returned a handle: is it an existing object?
fn existed() -> bool {
    // SAFETY: no arguments; read right after the create call.
    unsafe { GetLastError() == ERROR_ALREADY_EXISTS }
}

fn new_mapping(name: &str, size: usize) -> Result<Mapping, CreateError> {
    let w = wide(name);
    // SAFETY: a named, paging-file-backed, read/write mapping of `size` bytes.
    let h = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE,
            (size as u64 >> 32) as u32,
            size as u32,
            w.as_ptr(),
        )
    };
    if h.is_null() {
        return Err(win32("CreateFileMappingW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    let view =
        View::map_as(&handle, FILE_MAP_READ | FILE_MAP_WRITE, name).map_err(Win32Error::from)?;
    if view.len < size {
        return Err(Win32Error {
            what: format!("{name} maps {} bytes, under {size}", view.len),
            code: 0,
        }
        .into());
    }
    Ok(Mapping {
        _handle: handle,
        view,
    })
}

fn new_mutex(name: &str) -> Result<Handle, CreateError> {
    let w = wide(name);
    // SAFETY: a named mutex, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("CreateMutexW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    Ok(handle)
}

fn new_event(name: &str, manual_reset: bool) -> Result<Handle, CreateError> {
    let w = wide(name);
    // SAFETY: a named event, initially unsignaled; manual-reset or auto-reset as asked.
    let h = unsafe { CreateEventW(std::ptr::null(), manual_reset as i32, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("CreateEventW", name).into());
    }
    let handle = Handle(h);
    if existed() {
        return Err(CreateError::Exists(name.to_string()));
    }
    Ok(handle)
}

/// The producer's clock in ms (design: `ts` is `QueryPerformanceCounter`).
struct Clock {
    per_second: i64,
}

impl Clock {
    fn new() -> Clock {
        let mut f = 0i64;
        // SAFETY: writes one i64; it never fails on Windows XP and later.
        unsafe { QueryPerformanceFrequency(&mut f) };
        Clock {
            per_second: f.max(1),
        }
    }

    fn now_ms(&self) -> u64 {
        let mut c = 0i64;
        // SAFETY: writes one i64.
        unsafe { QueryPerformanceCounter(&mut c) };
        (c.max(0) as u128 * 1000 / self.per_second as u128) as u64
    }
}

/// The in-game side of one channel. Dropping it sets `disconnect` and resets `connect`.
pub struct Producer {
    outbox: Outbox,
    producer_id: u64,
    clock: Clock,
    _hello: Mapping,
    info: Mapping,
    events: Mapping,
    info_lock: Handle,
    events_lock: Handle,
    info_bell: Handle,
    events_bell: Handle,
    disconnect: Handle,
    connect: Handle,
}

impl Producer {
    /// Create channel `channel` of this process (design flow step 1), in order: the hello block,
    /// the info store with its schema written in, the event ring, both locks, both bells,
    /// `disconnect` and then `connect`. The hello block is written last, then `connect` is set.
    /// If any object already exists, it fails with `Exists`.
    pub fn create(channel: u32, schema: Schema) -> Result<Producer, CreateError> {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let name = |o| data_name(pid, channel, o);
        let outbox = Outbox::new(schema);
        let mut hello = new_mapping(&name(DataObject::Hello), HELLO_SIZE)?;
        let mut info = new_mapping(&name(DataObject::Info), outbox.info_layout().size())?;
        // No helper reads before `connect` is set, so the regions are written without the locks.
        info_init(info.bytes_mut(), outbox.schema(), outbox.info_layout());
        let mut events = new_mapping(&name(DataObject::Events), RING_SIZE)?;
        ring_init(events.bytes_mut());
        let info_lock = new_mutex(&name(DataObject::InfoLock))?;
        let events_lock = new_mutex(&name(DataObject::EventsLock))?;
        let info_bell = new_event(&name(DataObject::InfoBell), false)?;
        let events_bell = new_event(&name(DataObject::EventsBell), false)?;
        let disconnect = new_event(&name(DataObject::Disconnect), true)?;
        let connect = new_event(&name(DataObject::Connect), true)?;
        // SAFETY: no arguments.
        let producer_id = unsafe { GetTickCount64() };
        write_hello(hello.bytes_mut(), pid, producer_id);
        // SAFETY: signals our event handle.
        unsafe { SetEvent(connect.0) };
        Ok(Producer {
            outbox,
            producer_id,
            clock: Clock::new(),
            _hello: hello,
            info,
            events,
            info_lock,
            events_lock,
            info_bell,
            events_bell,
            disconnect,
            connect,
        })
    }

    /// This producer's id (the hello block's `producer_id`).
    pub fn producer_id(&self) -> u64 {
        self.producer_id
    }

    /// One info transaction: `(category, key, value)` entries, `None` for null. It is written at
    /// once if the info lock is free (then `info-bell` rings once, if anything changed), and on a
    /// later `commit` or `flush` otherwise.
    pub fn commit(&mut self, entries: &[(&str, &str, Option<&str>)]) -> Result<(), ProduceError> {
        self.outbox.commit(entries, self.clock.now_ms())?;
        self.write_info();
        Ok(())
    }

    /// Queue an event; `flush` writes it.
    pub fn push_event(&mut self, name: &str, data: &str) -> Result<(), ProduceError> {
        self.outbox.push_event(name, data, self.clock.now_ms())
    }

    /// Write pending info and queued events, within the throttle. Call it regularly.
    pub fn flush(&mut self) {
        self.write_info();
        self.write_events();
    }

    /// Events waiting for the ring.
    pub fn queued(&self) -> usize {
        self.outbox.queued()
    }

    fn write_info(&mut self) {
        if !self.outbox.has_pending_info() {
            return;
        }
        let Some(guard) = lock(&self.info_lock, 0) else {
            return;
        };
        let changed = self.outbox.write_info(self.info.bytes_mut());
        drop(guard);
        if changed {
            // SAFETY: signals our event handle.
            unsafe { SetEvent(self.info_bell.0) };
        }
    }

    fn write_events(&mut self) {
        if !self.outbox.has_pending_events() {
            return;
        }
        let Some(guard) = lock(&self.events_lock, 0) else {
            return;
        };
        let changed = self
            .outbox
            .write_events(self.events.bytes_mut(), self.clock.now_ms());
        drop(guard);
        if changed {
            // SAFETY: signals our event handle.
            unsafe { SetEvent(self.events_bell.0) };
        }
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        // SAFETY: sets and resets our own event handles; the handles close after this.
        unsafe {
            SetEvent(self.disconnect.0);
            ResetEvent(self.connect.0);
        }
    }
}

/// Why `Consumer::open` did not open the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No producer yet: `connect` does not exist or is not set. Retry later.
    NotReady,
    Win32(Win32Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotReady => f.write_str("no producer yet"),
            OpenError::Win32(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for OpenError {}

impl From<Win32Error> for OpenError {
    fn from(e: Win32Error) -> OpenError {
        OpenError::Win32(e)
    }
}

/// Why a consumer read gave nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeError {
    /// The lock was not acquired within `LOCK_TIMEOUT_MS`: skip, and retry on the next pass.
    Busy,
    /// The bytes do not fit (design "Failures"): report once, and wait for `disconnect`.
    Read(ReadError),
}

impl std::fmt::Display for ConsumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsumeError::Busy => f.write_str("lock busy"),
            ConsumeError::Read(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for ConsumeError {}

impl From<ReadError> for ConsumeError {
    fn from(e: ReadError) -> ConsumeError {
        ConsumeError::Read(e)
    }
}

/// What woke `Consumer::wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Disconnect,
    Info,
    Events,
    Timeout,
}

/// A checked attach (design flow steps 2-4): what `data_connected` and the snapshot report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub producer_id: u64,
    /// The seq after which events will be delivered.
    pub from_seq: u64,
    pub schema: Schema,
    /// Every key that has been set, nulls included.
    pub snapshot: Vec<InfoValue>,
}

/// The helper's side of one channel. Single-threaded: its data thread waits, reads and drops it.
pub struct Consumer {
    game_pid: u32,
    hello: Mapping,
    info: Mapping,
    events: Mapping,
    info_lock: Handle,
    events_lock: Handle,
    disconnect: Handle,
    info_bell: Handle,
    events_bell: Handle,
    _connect: Handle,
    reader: Option<InfoReader>,
    ring: Option<RingReader>,
}

fn open_event(name: &str) -> Result<Handle, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named event, to wait on it only.
    let h = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, w.as_ptr()) };
    if h.is_null() {
        Err(win32("OpenEventW", name))
    } else {
        Ok(Handle(h))
    }
}

fn open_mutex(name: &str) -> Result<Handle, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named mutex to wait on it and release it.
    let h = unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
            0,
            w.as_ptr(),
        )
    };
    if h.is_null() {
        Err(win32("OpenMutexW", name))
    } else {
        Ok(Handle(h))
    }
}

fn open_mapping(name: &str) -> Result<Mapping, Win32Error> {
    let w = wide(name);
    // SAFETY: opens an existing named mapping for reading only.
    let h = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, w.as_ptr()) };
    if h.is_null() {
        return Err(win32("OpenFileMappingW", name));
    }
    let handle = Handle(h);
    let view = View::map_as(&handle, FILE_MAP_READ, name)?;
    Ok(Mapping {
        _handle: handle,
        view,
    })
}

impl Consumer {
    /// Open channel `channel` of the game `pid` once its producer is ready: `connect` must exist
    /// and be set (design flow step 2). Then every other object is opened, and the three
    /// mappings are mapped read-only. Nothing is checked yet: see `attach`.
    pub fn open(pid: u32, channel: u32) -> Result<Consumer, OpenError> {
        let name = |o| data_name(pid, channel, o);
        let connect = match open_event(&name(DataObject::Connect)) {
            Ok(h) => h,
            Err(e) if e.code == ERROR_FILE_NOT_FOUND => return Err(OpenError::NotReady),
            Err(e) => return Err(e.into()),
        };
        // SAFETY: polls our event handle without waiting.
        if unsafe { WaitForSingleObject(connect.0, 0) } != WAIT_OBJECT_0 {
            return Err(OpenError::NotReady);
        }
        Ok(Consumer {
            game_pid: pid,
            hello: open_mapping(&name(DataObject::Hello))?,
            info: open_mapping(&name(DataObject::Info))?,
            events: open_mapping(&name(DataObject::Events))?,
            info_lock: open_mutex(&name(DataObject::InfoLock))?,
            events_lock: open_mutex(&name(DataObject::EventsLock))?,
            disconnect: open_event(&name(DataObject::Disconnect))?,
            info_bell: open_event(&name(DataObject::InfoBell))?,
            events_bell: open_event(&name(DataObject::EventsBell))?,
            _connect: connect,
            reader: None,
            ring: None,
        })
    }

    /// Check the hello block (magic, version, the game's pid), then under the info lock check
    /// the store and take the snapshot, then under the event lock check the ring and pick where
    /// events start: after `resume` if it names this producer, else at the write head. Calling it
    /// again starts over.
    pub fn attach(&mut self, resume: Option<ResumePoint>) -> Result<Attached, ConsumeError> {
        let hello = read_hello(self.hello.bytes(), self.game_pid)?;
        let (reader, snapshot) = {
            let _g = lock(&self.info_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
            let region = self.info.bytes();
            let mut reader = InfoReader::attach(region)?;
            let snapshot = reader.snapshot(region)?;
            (reader, snapshot)
        };
        let (ring, from_seq) = {
            let _g = lock(&self.events_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
            RingReader::start(self.events.bytes(), hello.producer_id, resume)?
        };
        let schema = reader.schema().clone();
        self.reader = Some(reader);
        self.ring = Some(ring);
        Ok(Attached {
            producer_id: hello.producer_id,
            from_seq,
            schema,
            snapshot,
        })
    }

    /// Under the info lock: the keys whose timestamps moved. Empty before `attach`.
    pub fn changes(&mut self) -> Result<Vec<InfoValue>, ConsumeError> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(Vec::new());
        };
        let _g = lock(&self.info_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
        Ok(reader.changes(self.info.bytes())?)
    }

    /// Under the event lock: the events since the last drain, in seq order, and what was lost
    /// before them. Empty before `attach`.
    pub fn drain(&mut self) -> Result<Drained, ConsumeError> {
        let Some(ring) = self.ring.as_mut() else {
            return Ok(Drained::default());
        };
        let _g = lock(&self.events_lock, LOCK_TIMEOUT_MS).ok_or(ConsumeError::Busy)?;
        Ok(ring.drain(self.events.bytes())?)
    }

    /// Wait up to `timeout_ms` on `[disconnect, info-bell, events-bell]`. `disconnect` has the
    /// smallest index, so it wins a tie (design "Platform facts").
    pub fn wait(&self, timeout_ms: u32) -> Wake {
        let handles = [self.disconnect.0, self.info_bell.0, self.events_bell.0];
        // SAFETY: waits on three handles this consumer owns; they are closed only when it drops.
        let r = unsafe { WaitForMultipleObjects(3, handles.as_ptr(), 0, timeout_ms) };
        match r {
            WAIT_OBJECT_0 => Wake::Disconnect,
            r if r == WAIT_OBJECT_0 + 1 => Wake::Info,
            r if r == WAIT_OBJECT_0 + 2 => Wake::Events,
            WAIT_TIMEOUT => Wake::Timeout,
            _ => {
                // A failed wait: sleep the timeout instead of spinning, then poll as on a timeout.
                // SAFETY: a plain sleep.
                unsafe { Sleep(timeout_ms) };
                Wake::Timeout
            }
        }
    }
}
```

- [ ] **Step 5: Build, clippy and rustfmt**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
cargo clippy --target x86_64-pc-windows-gnu -p uw-game-channel --all-targets -- -D warnings
cargo clippy -p uw-game-channel --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-game-channel/src/data.rs crates/uw-game-channel/examples/data_producer_probe.rs crates/uw-game-channel/examples/data_consumer_probe.rs crates/uw-game-channel/tests/wine_data.rs
```
Expected: `Finished` each time, no warnings; `target/x86_64-pc-windows-gnu/release/examples/` holds `data_producer_probe.exe` and `data_consumer_probe.exe` next to 2a's two probes; rustfmt prints nothing.

- [ ] **Step 6: Run the gate, and 2a's gate (GATE)**

Run:
```bash
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_data -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: `the_data_channel_works_between_two_wine_processes ... ok`, `test result: ok. 1 passed` (about 5 s); 2a's `the_channel_objects_work_between_two_wine_processes ... ok`, `1 passed` (the `objects.rs` edits change no behaviour); the leftover check prints nothing.

**If the data gate fails, STOP.** Do not go on to Task 6: report to the owner which answer differed (the assertion shows the probe's line and the expected one), with the test output. The helper's data thread depends on these objects.

- [ ] **Step 7: Commit**

```bash
git add crates/uw-game-channel
git commit -m "data: prove the data channel's producer and consumer between two Wine processes"
```

- [ ] **Step 8: Check that the gate bites, then restore**

In `crates/uw-game-channel/src/data.rs`, replace `    (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(Locked(mutex))` with `    (r == WAIT_OBJECT_0).then_some(Locked(mutex))` (an abandoned lock treated as busy). Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_data -- --ignored --test-threads=1
git checkout -- crates/uw-game-channel/src/data.rs
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_data -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: the mutated build warns `unused import: WAIT_ABANDONED` (expected); its test run FAILS at the abandoned events lock with `left: "busy 0"`, `right: "drain lost=0"`; after the restore it passes again (`1 passed`); `git status` is clean; no leftovers.

---

## Task 6: The data channel's JSON events

Design "New JSON events, helper → host": `data_connected` (`producer_id`, `from_seq`, `schema`), `info` (`value` a string or `null`), `data_event`, `data_lost` and `data_disconnected`, each with `channel`, plus `error` `data_version_mismatch` / `data_bad_layout` with the channel id as `detail` (`Event::data_error`). They carry no `request_id`, so `request_id()` is `None` and they never touch pending commands. The new variants go before `uw-host`'s own four, which stay last. The schema reuses Task 1's `SchemaCategory`/`SchemaKey`.

**Files:**
- Modify: `crates/uw-pipe-proto/src/jsonl.rs`, `crates/uw-pipe-proto/tests/jsonl.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/uw-pipe-proto/tests/jsonl.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use uw_pipe_proto::jsonl::{Command, Event};

```

with:

```rust
use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::{Command, Event};

```

2. Replace:

```rust
    assert!(Command::parse("not json").is_err());
}
```

with:

```rust
    assert!(Command::parse("not json").is_err());
}

fn data_events() -> Vec<Event> {
    vec![
        Event::DataConnected {
            channel: 1,
            producer_id: 123_456,
            from_seq: 7,
            schema: vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])],
        },
        Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: Some("3".into()), ts: 40 },
        Event::Info { channel: 1, category: "test".into(), key: "last_seq".into(), value: None, ts: 41 },
        Event::DataEvent { channel: 1, seq: 8, name: "tick".into(), data: "3".into(), ts: 42 },
        Event::DataLost { channel: 1, events: 12 },
        Event::DataDisconnected { channel: 1 },
        Event::data_error("data_bad_layout", 1),
    ]
}

/// Catches renamed data events or fields, or a null value left out instead of sent as `null`:
/// these are the wire forms in the design's "New JSON events" table.
#[test]
fn data_events_have_the_design_wire_form() {
    let lines: Vec<String> = data_events().iter().map(Event::to_line).collect();
    assert_eq!(
        lines,
        [
            r#"{"event":"data_connected","channel":1,"producer_id":123456,"from_seq":7,"schema":[{"category":"test","keys":[{"key":"ticks","max_len":32},{"key":"last_seq","max_len":32}]}]}"#,
            r#"{"event":"info","channel":1,"category":"test","key":"ticks","value":"3","ts":40}"#,
            r#"{"event":"info","channel":1,"category":"test","key":"last_seq","value":null,"ts":41}"#,
            r#"{"event":"data_event","channel":1,"seq":8,"name":"tick","data":"3","ts":42}"#,
            r#"{"event":"data_lost","channel":1,"events":12}"#,
            r#"{"event":"data_disconnected","channel":1}"#,
            r#"{"event":"error","code":"data_bad_layout","detail":1}"#,
        ]
    );
}

/// Catches a data event that does not parse back, or one taken for a reply (design: data events
/// carry no `request_id`, so they never touch pending commands).
#[test]
fn data_events_round_trip_and_are_not_replies() {
    for e in data_events() {
        assert_eq!(Event::parse(&e.to_line()).unwrap(), e);
        assert_eq!(e.request_id(), None, "{e:?}");
        assert!(e.clone().request_id_mut().is_none(), "{e:?}");
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p uw-pipe-proto --test jsonl`
Expected: does not compile: `` error[E0599]: no variant named `DataConnected` found for enum `Event` ``, the same for `Info`, `DataEvent`, `DataLost` and `DataDisconnected`, and `` no variant, associated function, or constant named `data_error` ``.

- [ ] **Step 3: Add the events**

In `crates/uw-pipe-proto/src/jsonl.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! Commands travel on the helper's stdin, events on its stdout (diagnostics on stderr). Every
//! command carries a `request_id`, and its one reply (`status`, `state_set`, `pong`, or an `error`)
//! echoes it. `uw-host` adds its own events (the last four `Event` variants) to the stream it
//! hands its user. Serialized with serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};

/// Host -> helper.
```

with:

```rust
//! Commands travel on the helper's stdin, events on its stdout (diagnostics on stderr). Every
//! command carries a `request_id`, and its one reply (`status`, `state_set`, `pong`, or an `error`)
//! echoes it. The game-data channel's events (design 2b: `data_connected`, `info`, `data_event`,
//! `data_lost`, `data_disconnected`) carry its `channel` and no `request_id`. `uw-host` adds its
//! own events (the last four `Event` variants) to the stream it hands its user. Serialized with
//! serde; `to_line` never contains a newline.

use serde::{Deserialize, Serialize};

use crate::data::SchemaCategory;

/// Host -> helper.
```

2. Replace:

```rust
    /// The game process ended (the helper then exits 0).
    GameExited,
    /// A failure. `request_id` is set when it answers a command; `detail` carries a
    /// `GetLastError` where there is one.
```

with:

```rust
    /// The game process ended (the helper then exits 0).
    GameExited,
    /// Data channel `channel` was opened and checked (design 2b): its producer, the seq after
    /// which events will be delivered, and its schema. The snapshot's `info` lines follow.
    DataConnected { channel: u32, producer_id: u64, from_seq: u64, schema: Vec<SchemaCategory> },
    /// One info key: the snapshot after `data_connected`, then each change. `value` null is a
    /// key set to null.
    Info { channel: u32, category: String, key: String, value: Option<String>, ts: u64 },
    /// One event from the ring, in seq order.
    DataEvent { channel: u32, seq: u64, name: String, data: String, ts: u64 },
    /// `events` were lost before the events that follow: overwritten before they were read, plus
    /// growth in the producer's dropped count.
    DataLost { channel: u32, events: u64 },
    /// The producer set `disconnect`.
    DataDisconnected { channel: u32 },
    /// A failure. `request_id` is set when it answers a command; `detail` carries a
    /// `GetLastError` where there is one.
```

3. Replace:

```rust
    }

    /// The `request_id` a reply answers (`status`, `state_set`, `pong`, or an `error` that
    /// answers a command); `None` for an unsolicited event.
```

with:

```rust
    }

    /// A data channel error (`data_version_mismatch`, `data_bad_layout`): `detail` is the
    /// channel id (design 2b).
    pub fn data_error(code: &str, channel: u32) -> Event {
        Event::Error { request_id: None, code: code.to_string(), detail: Some(channel) }
    }

    /// The `request_id` a reply answers (`status`, `state_set`, `pong`, or an `error` that
    /// answers a command); `None` for an unsolicited event.
```

- [ ] **Step 4: Run the tests and clippy**

Run:
```bash
cargo test -p uw-pipe-proto
cargo clippy --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
```
Expected, all passing: lib unit tests 3, `bells` 3, `data_info` 6, `data_outbox` 5, `data_ring` 11, `data_schema` 6, `jsonl` 10, `pipe` 19, `state` 5; both clippies clean on all eight crates (every `match` on `Event` elsewhere has a catch-all arm).

- [ ] **Step 5: Commit**

```bash
git add crates/uw-pipe-proto
git commit -m "data: the data channel's JSON events"
```

---

## Task 7: The helper's data thread, its arguments, and `--bad-data-channel`

Design "Components and flow" steps 2-6, "Threads", "Helper arguments" and the "Failures" rows for versions and layouts. The helper's new `data.rs` is its only reader of the channel, on a thread of its own started with the relay; it shares only `emit()` (whole lines under one lock) with 2a's relay loop:
1. every second it tries `Consumer::open` (no producer yet: it says nothing);
2. `attach`, then `data_connected` and one `info` per key in the snapshot;
3. waits on `[disconnect, info-bell, events-bell]` for 1 s: `info` lines on `info-bell`, a drain on `events-bell` (`data_lost` first when anything was lost), both on a timeout; a read a busy lock skipped is owed and done on the next pass, whatever wakes it (`uw_helper::data_pass`, pure and unit-tested; see "Decisions");
4. on `disconnect`, `data_disconnected`, then it closes everything (only after its last wait returned) and starts over.

A version or layout that does not fit is reported once per producer (`error`, `detail` the channel id, the reason on stderr); then it only waits for `disconnect`. `main.rs` parses `--data-channel <n>` (default 1) and `--data-resume <producer_id>:<seq>`; a malformed value is a usage error, exit 2. The test game's `--bad-data-channel <version|layout>` holds a channel whose hello block has format version 2 or a wrong magic (see "Decisions"). The helper suite's reader now puts data events on a channel of their own (`Helper::data`, `next_data`, `expect_data_quiet`), so 2a's tests never see them.

**Files:**
- Modify: `crates/uw-helper/src/lib.rs`, `crates/uw-helper/src/bin/uw-helper64/main.rs`, `crates/uw-helper/tests/wine_e2e.rs`, `crates/uw-test-game/Cargo.toml`, `crates/uw-test-game/src/main.rs`
- Create: `crates/uw-helper/src/data_pass.rs`, `crates/uw-helper/tests/data_pass.rs`, `crates/uw-helper/src/bin/uw-helper64/data.rs`

- [ ] **Step 1: Add `--bad-data-channel` and write the failing tests**

`crates/uw-helper/tests/data_pass.rs` (complete file):

```rust
use uw_helper::data_pass::{Owed, Woke};

/// Catches the data thread reading the wrong region on a wake (design 2b flow step 5: a bell
/// reads its own region, and a timeout reads both, since a signal can be missed while a lock
/// attempt times out).
#[test]
fn a_bell_reads_its_own_region_and_a_timeout_reads_both() {
    let owed = Owed::default();
    assert_eq!(owed.reads(Woke::InfoBell), (true, false));
    assert_eq!(owed.reads(Woke::EventsBell), (false, true));
    assert_eq!(owed.reads(Woke::Timeout), (true, true));
}

/// Catches a read skipped on a busy lock waiting for its own bell or a quiet second (design 2b
/// flow step 5: "skips and retries on the next pass"): with a producer that commits info all the
/// time, a skipped drain would otherwise wait for the next event, and a last event would sit in
/// the ring.
#[test]
fn a_read_a_busy_lock_skipped_is_retried_on_the_next_wake_of_any_kind() {
    let events_owed = Owed {
        info: false,
        events: true,
    };
    assert_eq!(events_owed.reads(Woke::InfoBell), (true, true));
    let info_owed = Owed {
        info: true,
        events: false,
    };
    assert_eq!(info_owed.reads(Woke::EventsBell), (true, true));
    let both = Owed {
        info: true,
        events: true,
    };
    assert_eq!(both.reads(Woke::InfoBell), (true, true));
}
```

In `crates/uw-test-game/Cargo.toml`, make this edit (each "replace" block is exact and occurs once):

1. Replace:

```toml
# message loop, so the helper's hook has a UI thread to target. `--no-window` sleeps instead
# (for the `no_window` test); `--bad-state-block` first creates and holds the state block with a
# layout v2 header (for the `version_mismatch` test). Built for x86_64-pc-windows-gnu.
[[bin]]
name = "uw-test-game"
```

with:

```toml
# message loop, so the helper's hook has a UI thread to target. `--no-window` sleeps instead
# (for the `no_window` test); `--bad-state-block` first creates and holds the state block with a
# layout v2 header (for the `version_mismatch` test); `--bad-data-channel <version|layout>` first
# creates and holds data channel 1 with a hello block the helper must refuse (for the
# `data_version_mismatch` and `data_bad_layout` tests). Built for x86_64-pc-windows-gnu.
[[bin]]
name = "uw-test-game"
```

In `crates/uw-test-game/src/main.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! `uw-test-game --bad-state-block` first creates `Local\underwolf-<pid>-state` with a layout v2
//! header and holds it, as a game would that kept a block from another client version (for the
//! `version_mismatch` scenario).

#[cfg(not(windows))]
```

with:

```rust
//! `uw-test-game --bad-state-block` first creates `Local\underwolf-<pid>-state` with a layout v2
//! header and holds it, as a game would that kept a block from another client version (for the
//! `version_mismatch` scenario). `uw-test-game --bad-data-channel <version|layout>` first creates
//! data channel 1's objects as a producer would, with a hello block of format version 2 or with a
//! wrong magic, and holds them with `connect` set (for the `data_version_mismatch` and
//! `data_bad_layout` scenarios).

#[cfg(not(windows))]
```

2. Replace:

```rust
    if std::env::args().any(|a| a == "--bad-state-block") {
        win::hold_bad_state_block();
    }
    if std::env::args().any(|a| a == "--no-window") {
```

with:

```rust
    if std::env::args().any(|a| a == "--bad-state-block") {
        win::hold_bad_state_block();
    }
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--bad-data-channel") {
        win::hold_bad_data_channel(args.get(i + 1).map_or("", String::as_str));
    }
    if std::env::args().any(|a| a == "--no-window") {
```

3. Replace:

```rust
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
```

with:

```rust
    }

    /// Create data channel 1's objects for our own pid, as a producer would (every mapping,
    /// lock, bell and both manual-reset events), with a hello block the helper must refuse:
    /// format version 2 (`version`) or a wrong magic (`layout`). `connect` is created set. The
    /// handles and the view are kept for the life of the process.
    pub fn hold_bad_data_channel(kind: &str) {
        use uw_pipe_proto::data::{write_hello, DEFAULT_CHANNEL, HELLO_SIZE};
        use uw_pipe_proto::names::{data_name, DataObject};
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_WRITE, PAGE_READWRITE};
        use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, GetCurrentProcessId};

        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        let name = |o| -> Vec<u16> { data_name(pid, DEFAULT_CHANNEL, o).encode_utf16().chain(Some(0)).collect() };
        // SAFETY: named paging-file mappings, mutexes and events with NUL-terminated names, and a
        // write view of the hello block; all deliberately kept open until the process exits.
        unsafe {
            let mappings = [(DataObject::Hello, HELLO_SIZE), (DataObject::Info, 4096), (DataObject::Events, 4096)];
            for (object, size) in mappings {
                let h = CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    0,
                    size as u32,
                    name(object).as_ptr(),
                );
                assert!(!h.is_null(), "CreateFileMappingW failed");
                if object == DataObject::Hello {
                    let view = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, 0);
                    assert!(!view.Value.is_null(), "MapViewOfFile failed");
                    let b = std::slice::from_raw_parts_mut(view.Value.cast::<u8>(), HELLO_SIZE);
                    write_hello(b, pid, 1);
                    match kind {
                        "version" => b[4..8].copy_from_slice(&2u32.to_le_bytes()),
                        "layout" => b[0] = b'X',
                        other => panic!("--bad-data-channel {other:?}: want version or layout"),
                    }
                }
            }
            for object in [DataObject::InfoLock, DataObject::EventsLock] {
                let h = CreateMutexW(std::ptr::null(), 0, name(object).as_ptr());
                assert!(!h.is_null(), "CreateMutexW failed");
            }
            for (object, manual, set) in [
                (DataObject::InfoBell, 0, 0),
                (DataObject::EventsBell, 0, 0),
                (DataObject::Disconnect, 1, 0),
                (DataObject::Connect, 1, 1),
            ] {
                let h = CreateEventW(std::ptr::null(), manual, set, name(object).as_ptr());
                assert!(!h.is_null(), "CreateEventW failed");
            }
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
```

In `crates/uw-helper/tests/wine_e2e.rs`, make these 6 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
/// A running `uw-helper64.exe`: its JSON-lines events arrive on a channel from a reader thread,
/// its stderr diagnostics accumulate in a shared buffer (so a test can assert which hook fired or
/// that the hook was skipped), commands go to its stdin. Dropping it kills the helper.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}
```

with:

```rust
/// A running `uw-helper64.exe`: its JSON-lines events arrive on a channel from a reader thread,
/// its stderr diagnostics accumulate in a shared buffer (so a test can assert which hook fired or
/// that the hook was skipped), commands go to its stdin. The game-data channel's events (design
/// 2b) arrive on a channel of their own, `data`, so the control-channel tests never see them.
/// Dropping it kills the helper.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<String>,
    data: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}
```

2. Replace:

```rust
    }

    /// Wait up to `within` for a stderr diagnostic line containing `needle`.
    fn wait_stderr(&self, needle: &str, within: Duration) -> bool {
```

with:

```rust
    }

    /// The next game-data event, parsed (a line that is not an event fails the test).
    fn next_data(&self, within: Duration) -> Event {
        let line = self
            .data
            .recv_timeout(within)
            .unwrap_or_else(|_| panic!("no data event within {within:?}; stderr: {:?}", self.stderr_lines()));
        Event::parse(&line).unwrap_or_else(|e| panic!("not an event ({e}): {line}"))
    }

    /// No game-data event within `d`.
    fn expect_data_quiet(&self, d: Duration) {
        if let Ok(line) = self.data.recv_timeout(d) {
            panic!("expected no data event, got {line}");
        }
    }

    /// Wait up to `within` for a stderr diagnostic line containing `needle`.
    fn wait_stderr(&self, needle: &str, within: Duration) -> bool {
```

3. Replace:

```rust
}

/// Start the helper against `uw-test-game.exe` in `pfx`, with extra args and environment.
fn start_helper(pfx: &Prefix, extra: &[&str], envs: &[(&str, &str)]) -> Helper {
```

with:

```rust
}

/// Whether an event line is the game-data channel's (design 2b): its five events, and the errors
/// whose code starts with `data_`.
fn is_data_line(line: &str) -> bool {
    match Event::parse(line) {
        Ok(
            Event::DataConnected { .. }
            | Event::Info { .. }
            | Event::DataEvent { .. }
            | Event::DataLost { .. }
            | Event::DataDisconnected { .. },
        ) => true,
        Ok(Event::Error { code, .. }) => code.starts_with("data_"),
        _ => false,
    }
}

/// Start the helper against `uw-test-game.exe` in `pfx`, with extra args and environment.
fn start_helper(pfx: &Prefix, extra: &[&str], envs: &[(&str, &str)]) -> Helper {
```

4. Replace:

```rust
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                break;
            }
```

with:

```rust
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    let (data_tx, data_rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.trim_start().starts_with('{') {
                continue;
            }
            let to = if is_data_line(&line) { &data_tx } else { &tx };
            if to.send(line).is_err() {
                break;
            }
```

5. Replace:

```rust
        }
    });
    Helper { child, stdin, events: rx, stderr: diag }
}

```

with:

```rust
        }
    });
    Helper { child, stdin, events: rx, data: data_rx, stderr: diag }
}

```

6. Replace:

```rust
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), applied(2));
}
```

with:

```rust
    assert_eq!(helper.next_parsed(Duration::from_secs(5)), applied(2));
}

// --- The game-data channel (plan 2b) ---

/// The helper with `args` must exit 2 at once and say `why` (design 2b: a malformed data argument
/// is a usage error, like the helper's other arguments).
fn assert_usage_error(pfx: &Prefix, args: &[&str], why: &str) {
    let mut h = start_helper(pfx, args, &[]);
    let status = h.wait_exit(Duration::from_secs(30));
    assert_eq!(status.and_then(|s| s.code()), Some(2), "{args:?}: stderr: {:?}", h.stderr_lines());
    assert!(h.wait_stderr(why, Duration::from_secs(2)), "{args:?}: stderr: {:?}", h.stderr_lines());
}

/// Catches a data channel of another format version being read (design 2b "Failures": `error
/// data_version_mismatch`, `detail` the channel id, once per producer, then nothing more from
/// it), the data thread not starting with the relay, and a malformed `--data-channel` or
/// `--data-resume` being accepted.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_data_channel_of_another_version_is_data_version_mismatch_once() {
    let mut pfx = setup("dataversion");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).args(["--bad-data-channel", "version"]).spawn().unwrap(),
    );
    wait_game_in(&pfx, Duration::from_secs(180));
    assert_usage_error(&pfx, &["--data-channel", "x"], "--data-channel needs a channel number");
    assert_usage_error(&pfx, &["--data-resume", "7"], r#"--data-resume: "7" is not <producer_id>:<seq>"#);
    assert_usage_error(&pfx, &["--data-resume"], "--data-resume needs <producer_id>:<seq>");

    let helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    assert_eq!(helper.next_data(Duration::from_secs(10)), Event::data_error("data_version_mismatch", 1));
    assert!(
        helper.wait_stderr("data channel 1: format version 2, not 1", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    // Once per producer: it now only waits for `disconnect`.
    helper.expect_data_quiet(Duration::from_secs(3));
}

/// Catches a data channel whose layout does not fit being read (design 2b "Failures": `error
/// data_bad_layout`, `detail` the channel id, once per producer, then nothing more from it).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_data_channel_with_a_bad_layout_is_data_bad_layout_once() {
    let mut pfx = setup("datalayout");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).args(["--bad-data-channel", "layout"]).spawn().unwrap(),
    );
    let helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    assert_eq!(helper.next_data(Duration::from_secs(10)), Event::data_error("data_bad_layout", 1));
    assert!(
        helper.wait_stderr("data channel 1: hello block magic is not UWDH", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    helper.expect_data_quiet(Duration::from_secs(3));
}
```

- [ ] **Step 2: Run them to see them fail**

Run:
```bash
cargo test -p uw-helper --test data_pass
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build --release --target x86_64-pc-windows-gnu -p uw-helper --example overlay_probe
timeout 600 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 a_data_channel
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: `data_pass` does not compile (`` error[E0432]: unresolved import `uw_helper::data_pass` ``). The Wine run gives `2 failed`: `a_data_channel_with_a_bad_layout_is_data_bad_layout_once` panics with `no data event within 10s` (the helper has no data thread); `a_data_channel_of_another_version_is_data_version_mismatch_once` fails in `assert_usage_error` (the old helper exits 2 with `unknown argument --data-channel`, not the new message); no leftovers.

- [ ] **Step 3: Add the reads per pass, the data thread and its arguments**

`crates/uw-helper/src/data_pass.rs` (complete file):

```rust
//! The data thread's reads on each pass (design 2b flow step 5), apart from the Win32 wait, so the
//! rule is unit-tested on Linux: a bell reads its own region and a timeout reads both, and a read
//! that a busy lock skipped stays owed until it succeeds, on the next pass whatever woke it.

/// What woke the data thread's wait, other than `disconnect` (which ends the relay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    InfoBell,
    EventsBell,
    Timeout,
}

/// The reads a busy lock skipped, still owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Owed {
    pub info: bool,
    pub events: bool,
}

impl Owed {
    /// The reads for a pass that `woke`, as `(info, events)`: the bell's own region (both on a
    /// timeout), and any read owed from an earlier pass.
    pub fn reads(&self, woke: Woke) -> (bool, bool) {
        let info = self.info || matches!(woke, Woke::InfoBell | Woke::Timeout);
        let events = self.events || matches!(woke, Woke::EventsBell | Woke::Timeout);
        (info, events)
    }
}
```

In `crates/uw-helper/src/lib.rs`, make this edit (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine, and `pending` the relay's `load_overlay` requests
//! awaiting their `status` (both unit-tested on Linux). The real `Win32` implementation, the pipe
//! servers and the relay loop live in the `cfg(windows)` bin `uw-helper64`.

pub mod decide;
pub mod pending;
```

with:

```rust
//! The portable half of the helper: `decide` holds the `Win32` trait and the
//! wait/hook/fallback/timeout state machine, `pending` the relay's `load_overlay` requests
//! awaiting their `status`, and `data_pass` the data thread's reads on each pass (all unit-tested
//! on Linux). The real `Win32` implementation, the pipe servers, the relay loop and the data
//! thread live in the `cfg(windows)` bin `uw-helper64`.

pub mod data_pass;
pub mod decide;
pub mod pending;
```

`crates/uw-helper/src/bin/uw-helper64/data.rs` (complete file):

```rust
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
//! 4. On `disconnect`, emit `data_disconnected`, close everything and go back to 1.
//!
//! A version or a layout that does not fit is reported once per producer (`error
//! data_version_mismatch` / `data_bad_layout`, `detail`: the channel id); then the thread stops
//! reading and waits for `disconnect`.

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

/// Start the data thread for channel `channel` of the game `pid`. `resume` is `--data-resume`.
pub(crate) fn spawn(pid: u32, channel: u32, resume: Option<ResumePoint>) {
    let started = std::thread::Builder::new()
        .name("uw-data".into())
        .spawn(move || run(pid, channel, resume));
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
            Wake::Disconnect => return Ok(()),
            Wake::Info => Woke::InfoBell,
            Wake::Events => Woke::EventsBell,
            Wake::Timeout => Woke::Timeout,
        };
        let (read_info, read_events) = owed.reads(woke);
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
    }
}

/// Read nothing more from this producer: only wait for its `disconnect`.
fn wait_for_disconnect(c: &Consumer) {
    while c.wait(WAIT_MS) != Wake::Disconnect {}
}
```

In `crates/uw-helper/src/bin/uw-helper64/main.rs`, make these 6 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback]`
//!
//! Runs inside the game's Wine session. Creates the control channel (doorbells, mask region,
```

with:

```rust
//! `uw-helper64 <game-exe> --bootstrap <windows path> [--fallback] [--data-channel <n>]
//!   [--data-resume <producer_id>:<seq>]`
//!
//! Runs inside the game's Wine session. Creates the control channel (doorbells, mask region,
```

2. Replace:

```rust
//! pinned, skips the hook), then serves both pipes and relays JSON lines: commands
//! (`load_overlay`, `set_state`, `ping`) on stdin, events on stdout, each reply echoing its
//! command's `request_id`; diagnostics on stderr.
//!
//! Modules: `win32` (the real `Win32` for `decide`, and the channel), `pipe_server` (the
//! overlapped pipe server both pipes use), `commands` (the stdin reader; stdin closing ends the
//! helper in every phase), `relay` (the one wait loop, and the game watcher).

#[cfg(not(windows))]
```

with:

```rust
//! pinned, skips the hook), then serves both pipes and relays JSON lines: commands
//! (`load_overlay`, `set_state`, `ping`) on stdin, events on stdout, each reply echoing its
//! command's `request_id`; diagnostics on stderr. With the relay it starts the data thread, which
//! reads the game-data channel `--data-channel` (default 1) and relays it on the same stdout,
//! resuming after `--data-resume` when that names the same producer (design 2b).
//!
//! Modules: `win32` (the real `Win32` for `decide`, and the channel), `pipe_server` (the
//! overlapped pipe server both pipes use), `commands` (the stdin reader; stdin closing ends the
//! helper in every phase), `relay` (the one wait loop, and the game watcher), `data` (the data
//! thread).

#[cfg(not(windows))]
```

3. Replace:

```rust
#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod pipe_server;
```

with:

```rust
#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod data;
#[cfg(windows)]
mod pipe_server;
```

4. Replace:

```rust
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
```

with:

```rust
fn helper_main() -> i32 {
    use uw_helper::decide::{run, Config, Outcome};
    use uw_pipe_proto::data::{ResumePoint, DEFAULT_CHANNEL};

    const USAGE: &str = "usage: uw-helper64 <game-exe> --bootstrap <path> [--fallback] \
                         [--data-channel <n>] [--data-resume <producer_id>:<seq>]";
    let mut args = std::env::args().skip(1);
    let exe = args.next().unwrap_or_default();
    let mut bootstrap = String::new();
    let mut force_fallback = false;
    let mut data_channel = DEFAULT_CHANNEL;
    let mut data_resume: Option<ResumePoint> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bootstrap" => bootstrap = args.next().unwrap_or_default(),
            "--fallback" => force_fallback = true,
            // A malformed value is a usage error, like the other arguments (design 2b).
            "--data-channel" => match args.next().and_then(|v| v.parse().ok()) {
                Some(n) => data_channel = n,
                None => {
                    eprintln!("[uw-helper64] --data-channel needs a channel number; {USAGE}");
                    return 2;
                }
            },
            "--data-resume" => match args.next().map(|v| v.parse::<ResumePoint>()) {
                Some(Ok(r)) => data_resume = Some(r),
                Some(Err(e)) => {
                    eprintln!("[uw-helper64] --data-resume: {e}; {USAGE}");
                    return 2;
                }
                None => {
                    eprintln!("[uw-helper64] --data-resume needs <producer_id>:<seq>; {USAGE}");
                    return 2;
                }
            },
            other => {
                eprintln!("[uw-helper64] unknown argument {other}");
```

5. Replace:

```rust
    }
    if exe.is_empty() || bootstrap.is_empty() {
        eprintln!("usage: uw-helper64 <game-exe> --bootstrap <path> [--fallback]");
        return 2;
    }
```

with:

```rust
    }
    if exe.is_empty() || bootstrap.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
```

6. Replace:

```rust
            let pid = w.pid;
            if let (Some(channel), Some(pipe)) = (w.channel.as_mut(), w.pipe.as_mut()) {
                relay::run(pid, &commands, channel, pipe, status_timeout_ms, ping_timeout_ms);
            }
```

with:

```rust
            let pid = w.pid;
            if let (Some(channel), Some(pipe)) = (w.channel.as_mut(), w.pipe.as_mut()) {
                // The data thread starts with the relay (design 2b flow step 2).
                data::spawn(pid, data_channel, data_resume);
                relay::run(pid, &commands, channel, pipe, status_timeout_ms, ping_timeout_ms);
            }
```

- [ ] **Step 4: Run the new tests, then both suites, clippy and rustfmt**

Run:
```bash
cargo test -p uw-helper
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
cargo clippy -p uw-helper -p uw-test-game --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-helper -p uw-test-game --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-helper/src/data_pass.rs crates/uw-helper/tests/data_pass.rs crates/uw-helper/src/bin/uw-helper64/data.rs
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: `uw-helper` unit tests all pass: `data_pass` 2, `decide` 30, `pending` 3; both clippies clean; rustfmt prints nothing; helper Wine `18 passed` (2a's 16 and the two new ones; about 95 s), `uw-inject` `2 passed`; no leftovers. 2a's tests still pass: with no producer the data thread only looks once a second.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-helper crates/uw-test-game
git commit -m "data: the helper's data thread, its arguments, and --bad-data-channel"
```

- [ ] **Step 6: Check that the owed-read rule bites, then restore**

In `crates/uw-helper/src/data_pass.rs`, replace `        let info = self.info || matches!(woke, Woke::InfoBell | Woke::Timeout);` with `        let info = matches!(woke, Woke::InfoBell | Woke::Timeout);` (a skipped info read waits for its own bell). Run:
```bash
cargo test -p uw-helper --test data_pass
git checkout -- crates/uw-helper/src/data_pass.rs
cargo test -p uw-helper --test data_pass
```
Expected: the first run FAILS `a_read_a_busy_lock_skipped_is_retried_on_the_next_wake_of_any_kind` with `left: (false, true)`, `right: (true, true)`; after the restore `2 passed`; `git status` is clean.

---

## Task 8: The test overlay's producer, and the end-to-end data flow

Design "Testing" 3: the test overlay becomes the test producer: data channel 1, category `test` with keys `ticks` and `last_seq` (32 bytes each; `last_seq` starts null), a `tick` event (data: the count) and a `ticks` commit every second, and after each acknowledged state a `state_seen` event (data: the seq) and a `last_seq` commit. It runs on a thread of its own, fed by the bell callback through a channel, and flushes every 100 ms. `UW_TEST_OVERLAY_DATA_RESTART_MS` drops the producer after that long and creates it again once the helper has let go of the old objects. The tests:
- helper: no data events before a producer exists; then `data_connected` with the test schema, the two-key snapshot (`last_seq` null), ticks with contiguous seqs after `from_seq`, and `set_state` giving `state_seen` and `last_seq`;
- helper: a producer that goes away gives `data_disconnected`, and the next one a new `data_connected`;
- helper: 2a's game-exit and stdin-after-connect tests now also require the data thread to be attached (the "Game exits" and "Stdin closes" rows);
- `uw-inject`: the data events reach its user, and a second `uw-inject` (a whole-client restart) starts at the write head with the full snapshot.

**Files:**
- Modify: `crates/uw-test-overlay/Cargo.toml`, `crates/uw-test-overlay/src/lib.rs`, `crates/uw-helper/tests/wine_e2e.rs`, `crates/uw-inject/tests/wine_e2e.rs`, `Cargo.lock`

- [ ] **Step 1: Write the failing tests**

In `crates/uw-helper/tests/wine_e2e.rs`, make these 5 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use std::time::{Duration, Instant, SystemTime};

use uw_pipe_proto::jsonl::{Command as Cmd, Event};

```

with:

```rust
use std::time::{Duration, Instant, SystemTime};

use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::{Command as Cmd, Event};

```

2. Replace:

```rust
    helper.send(&load_overlay_line(&overlay));
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));

    // Kill the game while the helper is idle; the watcher thread must notice and report
```

with:

```rust
    helper.send(&load_overlay_line(&overlay));
    helper.expect_event(r#""code":"ok""#, Duration::from_secs(30));
    // The data thread relays the test producer (design 2b "Failures": `game_exited` still stops
    // everything).
    assert!(matches!(helper.next_data(Duration::from_secs(10)), Event::DataConnected { .. }));

    // Kill the game while the helper is idle; the watcher thread must notice and report
```

3. Replace:

```rust
}

/// The same once the bootstrap is connected and the relay runs.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
```

with:

```rust
}

/// The same once the bootstrap is connected and the relay runs, and while the data thread relays
/// the test producer (design 2b "Failures": stdin closing ends the helper in every phase).
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
```

4. Replace:

```rust
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
```

with:

```rust
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    assert!(matches!(helper.next_data(Duration::from_secs(10)), Event::DataConnected { .. }));
    helper.close_stdin();
    let status = helper.wait_exit(Duration::from_secs(10));
```

5. Replace:

```rust
    );
    helper.expect_data_quiet(Duration::from_secs(3));
}
```

with:

```rust
    );
    helper.expect_data_quiet(Duration::from_secs(3));
}

/// The test producer's schema (`uw-test-overlay`).
fn test_schema() -> Vec<SchemaCategory> {
    vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])]
}

/// Read data events until `done` holds for one; return them all.
fn data_until(helper: &Helper, within: Duration, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "data: no match within {within:?}; saw {seen:?}");
        let e = helper.next_data(left);
        let stop = done(&e);
        seen.push(e);
        if stop {
            return seen;
        }
    }
}

/// The seqs of the `data_event`s in `events`, in order.
fn event_seqs(events: &[Event]) -> Vec<u64> {
    events.iter().filter_map(|e| if let Event::DataEvent { seq, .. } = e { Some(*seq) } else { None }).collect()
}

/// Expect `data_connected` with the test schema, then the two-key snapshot. Returns the producer
/// id, `from_seq`, and the snapshot's `ticks` and `last_seq` values.
fn expect_test_producer(helper: &Helper) -> (u64, u64, String, Option<String>) {
    let connected = helper.next_data(Duration::from_secs(10));
    let Event::DataConnected { channel: 1, producer_id, from_seq, schema } = connected else {
        panic!("expected data_connected, got {connected:?}");
    };
    assert_eq!(schema, test_schema());
    let ticks = match helper.next_data(Duration::from_secs(5)) {
        Event::Info { channel: 1, category, key, value: Some(v), .. } if category == "test" && key == "ticks" => v,
        other => panic!("expected the ticks snapshot, got {other:?}"),
    };
    let last_seq = match helper.next_data(Duration::from_secs(5)) {
        Event::Info { channel: 1, key, value, .. } if key == "last_seq" => value,
        other => panic!("expected the last_seq snapshot, got {other:?}"),
    };
    (producer_id, from_seq, ticks, last_seq)
}

/// Catches the design's main data flow breaking at the helper (design 2b flow steps 2-5): data
/// events before any producer exists, no `data_connected` with the test schema once the overlay
/// creates it, a snapshot without both keys (`last_seq` still null), ticks that do not follow
/// `from_seq` with contiguous seqs, `ticks` info not following them, or no `state_seen` (and no
/// `last_seq`) after a `set_state`.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn the_data_thread_waits_for_the_producer_then_relays_it() {
    let mut pfx = setup("data");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let overlay = to_wine_path(&win_artifact("uw_test_overlay.dll"));
    let mut helper = start_helper(&pfx, &[], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(200));
    // No producer yet: the data thread looks once a second and says nothing.
    helper.expect_data_quiet(Duration::from_secs(3));
    helper.send_cmd(&load(&overlay));
    helper.expect_all(&[ok(1), Event::OverlayConnected { module_version: OVERLAY_VERSION }], Duration::from_secs(30));

    let (_, from_seq, ticks, snapshot_last_seq) = expect_test_producer(&helper);
    assert_eq!(snapshot_last_seq, None, "no state was seen yet");
    let ticks: u64 = ticks.parse().unwrap_or_else(|_| panic!("ticks {ticks:?}"));
    // Two more ticks: each a `tick` event (data: the count), after a `ticks` info with that count.
    let seen = data_until(&helper, Duration::from_secs(10), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "tick" && *data == (ticks + 2).to_string())
    });
    let numbers = |want_event: bool| -> Vec<u64> {
        seen.iter()
            .filter_map(|e| match e {
                Event::DataEvent { name, data, .. } if want_event && name == "tick" => data.parse().ok(),
                Event::Info { key, value: Some(v), .. } if !want_event && key == "ticks" => v.parse().ok(),
                _ => None,
            })
            .collect()
    };
    // Attach reads the snapshot under the info lock and the start point under the events lock, a
    // moment apart, and a tick commits `ticks` just before it writes its event. So the ring may be
    // a tick behind the snapshot (events from `ticks` on) or a tick ahead (only `ticks + 2`).
    let events = numbers(true);
    let fits = [vec![ticks + 1, ticks + 2], vec![ticks, ticks + 1, ticks + 2], vec![ticks + 2]];
    assert!(fits.contains(&events), "{seen:?}");
    assert_eq!(numbers(false), [ticks + 1, ticks + 2], "{seen:?}");

    // `set_state` -> `state_applied` -> a `state_seen` event and `last_seq`, both with seq 1 (the
    // info and the event come in either order).
    fn state_seen(e: &Event) -> bool {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    }
    fn last_seq(e: &Event) -> bool {
        matches!(e, Event::Info { key, value: Some(v), .. } if key == "last_seq" && v == "1")
    }
    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 2, seq: 1 }, applied(1)], Duration::from_secs(10));
    let mut more = data_until(&helper, Duration::from_secs(10), |e| state_seen(e) || last_seq(e));
    let rest: fn(&Event) -> bool = if more.iter().any(state_seen) { last_seq } else { state_seen };
    more.extend(data_until(&helper, Duration::from_secs(5), rest));
    let all = [seen, more].concat();
    let seqs = event_seqs(&all);
    let want: Vec<u64> = (from_seq + 1..).take(seqs.len()).collect();
    assert_eq!(seqs, want, "seqs skip or repeat: {all:?}");
}

/// Catches a producer that goes away not reported (design 2b flow step 6: `data_disconnected`),
/// or the data thread not closing everything and going back to waiting: the test producer, made
/// again in the same game, would then fail with `Exists` for good, and no second `data_connected`
/// (a new `producer_id`, the full snapshot, ticks from its own `from_seq`) would come.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_producer_that_goes_away_is_data_disconnected_and_the_next_one_connects() {
    let mut pfx = setup("datarestart");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe"))
            .env("UW_TEST_OVERLAY_DATA_RESTART_MS", "4000")
            .spawn()
            .unwrap(),
    );
    let helper = start_with_overlay(&pfx);
    let (first, _, _, _) = expect_test_producer(&helper);
    data_until(&helper, Duration::from_secs(15), |e| *e == Event::DataDisconnected { channel: 1 });
    let (second, from_seq, ticks, _) = expect_test_producer(&helper);
    assert_ne!(second, first, "the new producer has its own id");
    assert!(ticks.parse::<u64>().is_ok_and(|t| t >= 3), "the snapshot holds the current count: {ticks:?}");
    let seen =
        data_until(&helper, Duration::from_secs(5), |e| matches!(e, Event::DataEvent { name, .. } if name == "tick"));
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}
```

In `crates/uw-inject/tests/wine_e2e.rs`, make these 3 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. A helper
//! killed under a running `uw-inject` is restarted, and the overlay gets the reconciled state.
//! Ignored by default (needs Wine and the Windows artifacts).
//!
```

with:

```rust
//! loaded. A second `uw-inject` run (a client restart) reconnects without reloading. A helper
//! killed under a running `uw-inject` is restarted, and the overlay gets the reconciled state.
//! The test overlay's producer reaches `uw-inject`'s user through the game-data channel (2b).
//! Ignored by default (needs Wine and the Windows artifacts).
//!
```

2. Replace:

```rust

use uw_host::discovery::find_pids;
use uw_pipe_proto::jsonl::Event;

```

with:

```rust

use uw_host::discovery::find_pids;
use uw_pipe_proto::data::SchemaCategory;
use uw_pipe_proto::jsonl::Event;

```

3. Replace:

```rust
    assert!(pid_in(&fx.prefix, "uw-test-game.exe").is_some(), "the game stopped");
}
```

with:

```rust
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
```

- [ ] **Step 2: Run them to see them fail**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 the_data_thread a_producer_that closing_stdin_ends_the_helper_after killing_the_game
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1 uw_inject_relays
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: helper `4 failed`, each with `no data event within 10s` (the overlay has no producer yet); `uw-inject` `1 failed` with `no event in time` after 200 s (no `data_connected`); no leftovers.

- [ ] **Step 3: Make the test overlay a producer**

`crates/uw-test-overlay/Cargo.toml` (complete file):

```toml
[package]
name = "uw-test-overlay"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish.workspace = true

# A stand-in overlay for the Wine end-to-end tests. `uw_overlay_start` writes
# `%TEMP%\uw-test-overlay-<pid>.txt`, starts the control channel (`uw-game-channel`) and the test
# producer on data channel 1, and returns 0; on each bell it writes `%TEMP%\uw-test-state-<pid>.txt`
# and acknowledges the state. The tests read both files through the prefix's TEMP. Built for
# x86_64-pc-windows-gnu.
[lib]
name = "uw_test_overlay"
crate-type = ["cdylib"]

[dependencies]
uw-game-channel = { path = "../uw-game-channel" }
uw-pipe-proto = { path = "../uw-pipe-proto" }

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
//! (`uw_game_channel::start`, module version 1) and the test producer, and returns 0. On every
//! bell it writes `%TEMP%\uw-test-state-<pid>.txt` (`seq=<n> edit_mode=<0|1> visible=<0|1>`) and
//! then acknowledges that `seq` (`applied`), which rings `state_applied`. It ignores
//! `StartParams` (it reads its own pid), which is enough for the tests; the real overlay (plan 3)
//! reads them.
//!
//! The test producer (design 2b "Testing" 3) runs on a thread of its own: data channel 1, category
//! `test` with keys `ticks` and `last_seq` (32 bytes each, `last_seq` null at first). Every second
//! it pushes a `tick` event (data: the tick count) and commits `ticks`; after each acknowledged
//! state it pushes a `state_seen` event (data: the seq) and commits `last_seq`. It flushes every
//! 100 ms.
//!
//! In the game's environment, `UW_TEST_OVERLAY_START_DELAY_MS` makes `uw_overlay_start` take that
//! long (the late-reply test), and `UW_TEST_OVERLAY_BELL_DELAY_MS` makes every bell take that long
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).
//! `UW_TEST_OVERLAY_DATA_RESTART_MS` drops the producer after that long and creates it again once
//! the helper has let go of the old objects (the disconnect test).

#[cfg(windows)]
mod win {
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    use uw_game_channel::data::Producer;
    use uw_pipe_proto::data::{Schema, SchemaCategory};
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetTempPathW,
        WriteFile,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    const MODULE_VERSION: u32 = 1;
    const DATA_CHANNEL: u32 = 1;
    const FLUSH: Duration = Duration::from_millis(100);
    const TICK: Duration = Duration::from_secs(1);

    /// # Safety
    /// Called across the overlay FFI boundary; the pointer is ignored.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn uw_overlay_start(_params: *const core::ffi::c_void) -> u32 {
        // The overlay catches its own panics: a panic must not cross this `extern "system"`
        // boundary (it would abort; the bootstrap caller cannot catch it). 2 = panicked.
        std::panic::catch_unwind(run).unwrap_or(2)
    }

    /// A delay in ms from the environment variable `name`, if set.
    fn env_delay(name: &str) -> Option<Duration> {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).map(Duration::from_millis)
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
        let (seen_tx, seen_rx) = channel();
        let restart_after = env_delay("UW_TEST_OVERLAY_DATA_RESTART_MS");
        if std::thread::Builder::new()
            .name("uw-test-producer".into())
            .spawn(move || produce(&seen_rx, restart_after))
            .is_err()
        {
            return 4;
        }
        let started = uw_game_channel::start(MODULE_VERSION, move |link, _bits, state| {
            if let Some(d) = bell_delay {
                std::thread::sleep(d);
            }
            let text = format!("seq={} edit_mode={} visible={}\n", state.seq, state.edit_mode as u8, state.visible as u8);
            write_temp(&format!("uw-test-state-{pid}.txt"), &text);
            match link.applied(state.seq) {
                Ok(()) => {
                    let _ = seen_tx.send(state.seq);
                }
                Err(e) => eprintln!("[uw-test-overlay] applied: {e:?}"),
            }
        });
        if started.is_err() { 3 } else { 0 }
    }

    /// Create the test producer, retrying while the old objects still exist (a helper holds them
    /// until it sees `disconnect`), and commit its first values.
    fn create_producer(ticks: u64, last_seq: Option<u64>) -> Producer {
        let schema = Schema::new(vec![SchemaCategory::new("test", &[("ticks", 32), ("last_seq", 32)])])
            .expect("the test schema fits");
        let mut p = loop {
            match Producer::create(DATA_CHANNEL, schema.clone()) {
                Ok(p) => break p,
                Err(e) => {
                    eprintln!("[uw-test-overlay] data channel: {e}; retrying");
                    std::thread::sleep(FLUSH);
                }
            }
        };
        let (ticks, last_seq) = (ticks.to_string(), last_seq.map(|s| s.to_string()));
        if let Err(e) = p.commit(&[("test", "ticks", Some(&ticks)), ("test", "last_seq", last_seq.as_deref())]) {
            eprintln!("[uw-test-overlay] commit: {e}");
        }
        p
    }

    /// The test producer's thread: ticks every second, `state_seen` after each applied state,
    /// a flush every 100 ms.
    fn produce(seen: &Receiver<u64>, restart_after: Option<Duration>) {
        let mut ticks = 0u64;
        let mut last_seq = None;
        let mut p = create_producer(ticks, last_seq);
        let mut restart_at = restart_after.map(|d| Instant::now() + d);
        let mut next_tick = Instant::now() + TICK;
        loop {
            match seen.recv_timeout(FLUSH) {
                Ok(seq) => {
                    last_seq = Some(seq);
                    let seq = seq.to_string();
                    if let Err(e) = p.push_event("state_seen", &seq) {
                        eprintln!("[uw-test-overlay] push_event: {e}");
                    }
                    if let Err(e) = p.commit(&[("test", "last_seq", Some(&seq))]) {
                        eprintln!("[uw-test-overlay] commit: {e}");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // The worker's callback is gone: nothing more to report, but keep ticking.
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(FLUSH),
            }
            if Instant::now() >= next_tick {
                next_tick += TICK;
                ticks += 1;
                let n = ticks.to_string();
                if let Err(e) = p.push_event("tick", &n) {
                    eprintln!("[uw-test-overlay] push_event: {e}");
                }
                if let Err(e) = p.commit(&[("test", "ticks", Some(&n))]) {
                    eprintln!("[uw-test-overlay] commit: {e}");
                }
            }
            if restart_at.is_some_and(|t| Instant::now() >= t) {
                restart_at = None;
                drop(p);
                p = create_producer(ticks, last_seq);
            }
            p.flush();
        }
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

- [ ] **Step 4: Run both suites and clippy**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build -p uw-inject --bin uw-inject
cargo clippy -p uw-test-overlay -p uw-helper -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-test-overlay -p uw-helper -p uw-inject --all-targets -- -D warnings
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
git status --short
```
Expected: both clippies clean; helper `20 passed` (about 115 s; 2a's tests are untouched by the ticks, which go to `Helper::data`), `uw-inject` `3 passed`; no leftovers; `Cargo.lock` changed (the test overlay now depends on `uw-pipe-proto`).

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/uw-test-overlay crates/uw-helper crates/uw-inject
git commit -m "data: the test overlay's producer, and the end-to-end data flow tests"
```

---

## Task 9: Wine tests for resume, a burst under a slow host, a busy info lock and a broken store

The remaining "Failures" rows, at the helper:
- **Resume** ("Helper killed or restarted", "Resume names another producer"): a new helper with `--data-resume <producer_id>:<seq>` sends `data_connected` with that `from_seq` and every tick from its downtime once, in order; with another producer's id it starts at the write head.
- **A burst while the host reads slowly** ("Producer's dropped grew", "Host slow to read"): with `UW_TEST_OVERLAY_BURST=300` the producer queues 300 events of 1000 bytes before `state_seen`, so its queue of 256 drops at least 45. The test stops reading the helper's stdout for 3 s meanwhile (the pipe fills and `emit()` blocks the writer), then checks that every line parses, every seq gap was announced by a `data_lost` before it, the drops were reported, and the control replies came too.
- **A busy info lock** ("Lock busy for more than 100 ms"): `overlay_probe hold-info-lock 4000` holds the info lock and rings the info bell every 200 ms. A state applied meanwhile gets its `state_seen` event through, no `info` comes, and once the lock is released its `last_seq` arrives: the producer kept the value pending. `last_seq` is used because only a `state_seen` changes it; a `ticks` value can be overtaken by the next tick in the producer's first pass after the release (the plan review's I1: an earlier version of this test failed when the probe started 900 or 950 ms into a tick). The planner swept the probe's start over a whole tick, 0 to 950 ms in 50 ms steps: all 20 phases pass.
- **A store broken after `data_connected`** ("Bad layout", after attach): `overlay_probe break-info-magic` breaks the info store's magic under its lock. The helper reports `error data_bad_layout` (detail 1) once, then nothing for 3 s while the producer keeps ticking; when that producer goes away (`UW_TEST_OVERLAY_DATA_RESTART_MS=15000`) comes `data_disconnected`, and the next producer is relayed.

The code these tests check already exists (Tasks 3, 7 and 8), so the red runs are mutations (Step 4). The test aids are new: the burst variable, the two probe modes (`uw-helper` gets the `Win32_System_Memory` feature for the second), and a gate on the helper suite's stdout reader.

**Files:**
- Modify: `crates/uw-test-overlay/src/lib.rs`, `crates/uw-helper/Cargo.toml`, `crates/uw-helper/examples/overlay_probe.rs`, `crates/uw-helper/tests/wine_e2e.rs`

- [ ] **Step 1: Add the test aids and the tests**

In `crates/uw-test-overlay/src/lib.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).
//! `UW_TEST_OVERLAY_DATA_RESTART_MS` drops the producer after that long and creates it again once
//! the helper has let go of the old objects (the disconnect test).

#[cfg(windows)]
```

with:

```rust
//! to apply: the worker answers no `ping` meanwhile (the ping-timeout test).
//! `UW_TEST_OVERLAY_DATA_RESTART_MS` drops the producer after that long and creates it again once
//! the helper has let go of the old objects (the disconnect test). `UW_TEST_OVERLAY_BURST=<n>`
//! pushes n `burst` events (data: the count from 0, zero-padded to 1000 bytes) before each
//! `state_seen` (the burst test).

#[cfg(windows)]
```

2. Replace:

```rust
        let (seen_tx, seen_rx) = channel();
        let restart_after = env_delay("UW_TEST_OVERLAY_DATA_RESTART_MS");
        if std::thread::Builder::new()
            .name("uw-test-producer".into())
            .spawn(move || produce(&seen_rx, restart_after))
            .is_err()
        {
```

with:

```rust
        let (seen_tx, seen_rx) = channel();
        let restart_after = env_delay("UW_TEST_OVERLAY_DATA_RESTART_MS");
        let burst = std::env::var("UW_TEST_OVERLAY_BURST").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if std::thread::Builder::new()
            .name("uw-test-producer".into())
            .spawn(move || produce(&seen_rx, restart_after, burst))
            .is_err()
        {
```

3. Replace:

```rust
    }

    /// The test producer's thread: ticks every second, `state_seen` after each applied state,
    /// a flush every 100 ms.
    fn produce(seen: &Receiver<u64>, restart_after: Option<Duration>) {
        let mut ticks = 0u64;
        let mut last_seq = None;
```

with:

```rust
    }

    /// The test producer's thread: ticks every second, `state_seen` after each applied state
    /// (after `burst` events, if any), a flush every 100 ms.
    fn produce(seen: &Receiver<u64>, restart_after: Option<Duration>, burst: usize) {
        let mut ticks = 0u64;
        let mut last_seq = None;
```

4. Replace:

```rust
            match seen.recv_timeout(FLUSH) {
                Ok(seq) => {
                    last_seq = Some(seq);
                    let seq = seq.to_string();
```

with:

```rust
            match seen.recv_timeout(FLUSH) {
                Ok(seq) => {
                    for i in 0..burst {
                        if let Err(e) = p.push_event("burst", &format!("{i:0>1000}")) {
                            eprintln!("[uw-test-overlay] push_event: {e}");
                        }
                    }
                    last_seq = Some(seq);
                    let seq = seq.to_string();
```

In `crates/uw-helper/Cargo.toml`, make this edit (each "replace" block is exact and occurs once):

1. Replace:

```toml
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Pipes",
    "Win32_System_SystemInformation",
```

with:

```toml
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_Pipes",
    "Win32_System_SystemInformation",
```

In `crates/uw-helper/examples/overlay_probe.rs`, make these 5 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//!   set to 42 and `state_applied` rung, then act as the game side: answer each `bell-game`
//!   by applying the current `seq` and ringing `state_applied`, printing `bell <bits> seq <n>`.
//!
//! Runs under Wine in the game's prefix. It does nothing on other platforms.
```

with:

```rust
//!   set to 42 and `state_applied` rung, then act as the game side: answer each `bell-game`
//!   by applying the current `seq` and ringing `state_applied`, printing `bell <bits> seq <n>`.
//! - `hold-info-lock [ms]`: without touching the overlay pipe, hold the game's data channel 1
//!   info lock for `ms` (default 3000), ringing its info bell every 200 ms meanwhile; prints
//!   `locked`, then `released` (design 2b: a busy lock).
//! - `break-info-magic`: without touching the overlay pipe, break the magic of the game's data
//!   channel 1 info store, under its lock, as a producer gone wrong would; prints `broken`.
//!
//! Runs under Wine in the game's prefix. It does nothing on other platforms.
```

2. Replace:

```rust
    use uw_game_channel::objects::{LOCK_TIMEOUT_MS, Objects};
    use uw_pipe_proto::bells::to_host;
    use uw_pipe_proto::names::overlay_pipe_name;
    use uw_pipe_proto::pipe::{OverlayMessage, encode_overlay};
    use uw_pipe_proto::state;
```

with:

```rust
    use uw_game_channel::objects::{LOCK_TIMEOUT_MS, Objects};
    use uw_pipe_proto::bells::to_host;
    use uw_pipe_proto::data::DEFAULT_CHANNEL;
    use uw_pipe_proto::names::{DataObject, data_name, overlay_pipe_name};
    use uw_pipe_proto::pipe::{OverlayMessage, encode_overlay};
    use uw_pipe_proto::state;
```

3. Replace:

```rust
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Pipes::{PIPE_READMODE_MESSAGE, SetNamedPipeHandleState};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    fn say(s: &str) {
```

with:

```rust
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Memory::{
        FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    };
    use windows_sys::Win32::System::Pipes::{PIPE_READMODE_MESSAGE, SetNamedPipeHandleState};
    use windows_sys::Win32::System::Threading::{
        EVENT_MODIFY_STATE, MUTEX_MODIFY_STATE, OpenEventW, OpenMutexW, ReleaseMutex,
        SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
    };

    fn say(s: &str) {
```

4. Replace:

```rust
    }

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
```

with:

```rust
    }

    /// Hold the game's data channel 1 info lock for `ms`, ringing its info bell every 200 ms, as a
    /// producer that holds the lock too long would.
    fn hold_info_lock(pid: u32, ms: u64) -> i32 {
        let lock = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoLock));
        let bell = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoBell));
        // SAFETY: opens the game's named mutex and event; both names are NUL-terminated.
        let (l, b) = unsafe {
            (
                OpenMutexW(
                    SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                    0,
                    lock.as_ptr(),
                ),
                OpenEventW(EVENT_MODIFY_STATE, 0, bell.as_ptr()),
            )
        };
        if l.is_null() || b.is_null() {
            say("no data channel");
            return 7;
        }
        // SAFETY: waits on the mutex handle opened above.
        if unsafe { WaitForSingleObject(l, 5000) } != WAIT_OBJECT_0 {
            say("lock busy");
            return 8;
        }
        say("locked");
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(ms) {
            // SAFETY: signals the event handle opened above.
            unsafe { SetEvent(b) };
            std::thread::sleep(Duration::from_millis(200));
        }
        // SAFETY: releases the mutex this thread acquired above, then closes both handles.
        unsafe {
            ReleaseMutex(l);
            CloseHandle(l);
            CloseHandle(b);
        }
        say("released");
        0
    }

    /// Break the first byte of the game's data channel 1 info store, under the info lock.
    fn break_info_magic(pid: u32) -> i32 {
        let lock = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::InfoLock));
        let info = wide(&data_name(pid, DEFAULT_CHANNEL, DataObject::Info));
        // SAFETY: opens the game's named mutex and mapping; both names are NUL-terminated.
        let (l, m) = unsafe {
            (
                OpenMutexW(
                    SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                    0,
                    lock.as_ptr(),
                ),
                OpenFileMappingW(FILE_MAP_WRITE, 0, info.as_ptr()),
            )
        };
        if l.is_null() || m.is_null() {
            say("no data channel");
            return 7;
        }
        // SAFETY: waits on the mutex handle opened above.
        if unsafe { WaitForSingleObject(l, 5000) } != WAIT_OBJECT_0 {
            say("lock busy");
            return 8;
        }
        // SAFETY: maps the store for writing and, if that worked, writes its first byte while the
        // lock is held; then unmaps it, releases the lock and closes both handles.
        let mapped = unsafe {
            let view = MapViewOfFile(m, FILE_MAP_WRITE, 0, 0, 0);
            let mapped = !view.Value.is_null();
            if mapped {
                *view.Value.cast::<u8>() = b'X';
                UnmapViewOfFile(view);
            }
            ReleaseMutex(l);
            CloseHandle(l);
            CloseHandle(m);
            mapped
        };
        if !mapped {
            say("map failed");
            return 9;
        }
        say("broken");
        0
    }

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
```

5. Replace:

```rust
            return 4;
        };
        let p = connect(pid);
        match mode.as_str() {
```

with:

```rust
            return 4;
        };
        if mode == "hold-info-lock" {
            return hold_info_lock(pid, ms);
        }
        if mode == "break-info-magic" {
            return break_info_magic(pid);
        }
        let p = connect(pid);
        match mode.as_str() {
```

In `crates/uw-helper/tests/wine_e2e.rs`, make these 6 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
/// that the hook was skipped), commands go to its stdin. The game-data channel's events (design
/// 2b) arrive on a channel of their own, `data`, so the control-channel tests never see them.
/// Dropping it kills the helper.
struct Helper {
    child: Child,
```

with:

```rust
/// that the hook was skipped), commands go to its stdin. The game-data channel's events (design
/// 2b) arrive on a channel of their own, `data`, so the control-channel tests never see them.
/// While a test holds `stdout_gate`, the reader stops reading (a host slow to read). Dropping it
/// kills the helper.
struct Helper {
    child: Child,
```

2. Replace:

```rust
    events: Receiver<String>,
    data: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}
```

with:

```rust
    events: Receiver<String>,
    data: Receiver<String>,
    stdout_gate: Arc<Mutex<()>>,
    stderr: Arc<Mutex<Vec<String>>>,
}
```

3. Replace:

```rust
    }

    /// No game-data event within `d`.
    fn expect_data_quiet(&self, d: Duration) {
```

with:

```rust
    }

    /// Throw away the game-data events read so far.
    fn drain_data(&self) {
        while self.data.try_recv().is_ok() {}
    }

    /// No game-data event within `d`.
    fn expect_data_quiet(&self, d: Duration) {
```

4. Replace:

```rust
    let (tx, rx) = channel();
    let (data_tx, data_rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.trim_start().starts_with('{') {
                continue;
```

with:

```rust
    let (tx, rx) = channel();
    let (data_tx, data_rx) = channel();
    let stdout_gate = Arc::new(Mutex::new(()));
    let gate = Arc::clone(&stdout_gate);
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            // Wait here while a test holds the gate.
            drop(gate.lock().unwrap());
            let Some(Ok(line)) = lines.next() else { break };
            if !line.trim_start().starts_with('{') {
                continue;
```

5. Replace:

```rust
        }
    });
    Helper { child, stdin, events: rx, data: data_rx, stderr: diag }
}

```

with:

```rust
        }
    });
    Helper { child, stdin, events: rx, data: data_rx, stdout_gate, stderr: diag }
}

```

6. Replace:

```rust
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}
```

with:

```rust
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}

/// Catches `--data-resume` not honoured (design 2b "Failures": a new helper sends
/// `data_connected` with `from_seq` the resume seq, the full snapshot, then every event after it,
/// once each), and a resume point for another producer (a new game, a reused pid) not starting at
/// the write head.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_new_helper_resumes_after_the_given_seq_only_for_the_same_producer() {
    let mut pfx = setup("dataresume");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let (id, last) = {
        let helper = start_with_overlay(&pfx);
        let (id, from_seq, _, _) = expect_test_producer(&helper);
        data_until(&helper, Duration::from_secs(10), |e| {
            matches!(e, Event::DataEvent { seq, .. } if *seq == from_seq + 2)
        });
        (id, from_seq + 2)
    }; // the helper is killed here
    std::thread::sleep(Duration::from_secs(3)); // ticks go on while no helper runs

    let resume = format!("{id}:{last}");
    let helper = start_helper(&pfx, &["--data-resume", &resume], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(60));
    let (again, from_seq, _, _) = expect_test_producer(&helper);
    assert_eq!((again, from_seq), (id, last), "data_connected must resume at {resume}");
    let seen =
        data_until(&helper, Duration::from_secs(10), |e| matches!(e, Event::DataEvent { seq, .. } if *seq == last + 4));
    assert_eq!(event_seqs(&seen), (last + 1..=last + 4).collect::<Vec<_>>(), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, Event::DataLost { .. })), "{seen:?}");
    drop(helper);

    let other = format!("{}:0", id + 1);
    let helper = start_helper(&pfx, &["--data-resume", &other], &[]);
    helper.expect_event("bootstrap_connected", Duration::from_secs(60));
    let (_, from_seq, _, _) = expect_test_producer(&helper);
    assert!(from_seq > last + 4, "another producer's resume point must start at the write head, not {from_seq}");
    let seen = data_until(&helper, Duration::from_secs(5), |e| matches!(e, Event::DataEvent { .. }));
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}

/// Catches lines lost, split or reordered while the host reads slowly (design 2b "Failures":
/// `emit()` blocks the writer, and control replies still come), the producer's dropped count not
/// reaching `data_lost` (300 queued at once overflow the queue of 256 by at least 45), or a seq
/// gap not announced by a `data_lost` before the events after it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_burst_while_the_host_reads_slowly_loses_nothing_unannounced() {
    let mut pfx = setup("databurst");
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).env("UW_TEST_OVERLAY_BURST", "300").spawn().unwrap(),
    );
    let mut helper = start_with_overlay(&pfx);
    let (_, from_seq, _, _) = expect_test_producer(&helper);
    let gate = Arc::clone(&helper.stdout_gate);
    {
        // Stop reading the helper's stdout: its pipe fills and its writes block.
        let _paused = gate.lock().unwrap();
        helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
        helper.send_cmd(&Cmd::Ping { request_id: 3 });
        std::thread::sleep(Duration::from_secs(3));
    }
    // The control replies, late but whole.
    assert_eq!(helper.next_parsed(Duration::from_secs(30)), Event::StateSet { request_id: 2, seq: 1 });
    let rest = [helper.next_parsed(Duration::from_secs(30)), helper.next_parsed(Duration::from_secs(30))];
    assert!(rest.contains(&applied(1)), "{rest:?}");
    assert!(rest.iter().any(|e| matches!(e, Event::Pong { request_id: 3, .. })), "{rest:?}");

    let seen = data_until(&helper, Duration::from_secs(60), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    });
    let (mut prev, mut announced, mut lost, mut gaps) = (from_seq, 0u64, 0u64, 0u64);
    for e in &seen {
        match e {
            Event::DataLost { events, .. } => {
                announced += events;
                lost += events;
            }
            Event::DataEvent { seq, name, data, .. } => {
                let gap = seq.checked_sub(prev + 1).unwrap_or_else(|| panic!("seq {seq} after {prev}"));
                assert!(gap <= announced, "seq {seq} after {prev}: a gap of {gap}, but only {announced} announced");
                if name == "burst" {
                    assert!(data.len() == 1000 && data.parse::<u64>().is_ok(), "a burst event's data: {data:?}");
                }
                (prev, announced, gaps) = (*seq, 0, gaps + gap);
            }
            _ => {}
        }
    }
    assert!(lost - gaps >= 45, "the queue's drops were not reported: lost {lost}, gaps {gaps}");
}

/// Catches a busy info lock stalling the data thread or ending it (design 2b "Failures": a lock
/// busy for more than 100 ms is skipped and retried on the next pass), and a value committed
/// meanwhile being lost (design "A busy lock": the producer keeps the newest value and writes it
/// on the next call). The value is `last_seq`, which only a `state_seen` changes, so no tick can
/// overtake it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_busy_info_lock_is_skipped_and_the_newest_value_comes_after() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("databusy");
    pfx.game = Some(wine_cmd(&pfx, &win_artifact("uw-test-game.exe")).spawn().unwrap());
    let mut helper = start_with_overlay(&pfx);
    expect_test_producer(&helper);
    // The probe holds the info lock for 4 s and rings the info bell every 200 ms.
    let p = Probe::start(&pfx, &["hold-info-lock", "4000"]);
    p.expect_line("locked", Duration::from_secs(60));
    std::thread::sleep(Duration::from_millis(300));
    helper.drain_data();
    // A state applied while the info lock is held: its event gets through, its `last_seq` waits.
    helper.send_cmd(&Cmd::SetState { request_id: 2, edit_mode: Some(true), visible: None });
    helper.expect_all(&[Event::StateSet { request_id: 2, seq: 1 }, applied(1)], Duration::from_secs(10));
    let held = data_until(&helper, Duration::from_secs(3), |e| {
        matches!(e, Event::DataEvent { name, data, .. } if name == "state_seen" && data == "1")
    });
    assert!(!held.iter().any(|e| matches!(e, Event::Info { .. })), "info while the lock was held: {held:?}");
    p.expect_line("released", Duration::from_secs(10));
    data_until(&helper, Duration::from_secs(5), |e| {
        matches!(e, Event::Info { key, value: Some(v), .. } if key == "last_seq" && v == "1")
    });
}

/// Catches a store that stops fitting after `data_connected` being read on, reported again and
/// again, or re-attached (design 2b "Failures": `data_bad_layout` once per producer, then only the
/// wait for `disconnect`), and the next producer not being relayed after it.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_store_broken_after_data_connected_is_reported_once_until_disconnect() {
    win_artifact("examples/overlay_probe.exe"); // fail at once if the probe is not built
    let mut pfx = setup("databroken");
    // The producer goes away 15 s after it starts, and a new one comes.
    pfx.game = Some(
        wine_cmd(&pfx, &win_artifact("uw-test-game.exe"))
            .env("UW_TEST_OVERLAY_DATA_RESTART_MS", "15000")
            .spawn()
            .unwrap(),
    );
    let helper = start_with_overlay(&pfx);
    let (first, _, _, _) = expect_test_producer(&helper);
    let p = Probe::start(&pfx, &["break-info-magic"]);
    p.expect_line("broken", Duration::from_secs(60));
    // The next info read finds the broken header.
    data_until(&helper, Duration::from_secs(5), |e| *e == Event::data_error("data_bad_layout", 1));
    assert!(
        helper.wait_stderr("data channel 1: info store magic is not UWDI", Duration::from_secs(2)),
        "stderr: {:?}",
        helper.stderr_lines()
    );
    // Once per producer: nothing more from it, though it keeps ticking.
    helper.expect_data_quiet(Duration::from_secs(3));
    data_until(&helper, Duration::from_secs(20), |e| *e == Event::DataDisconnected { channel: 1 });
    let (second, _, _, _) = expect_test_producer(&helper);
    assert_ne!(second, first, "the next producer is relayed");
}
```

- [ ] **Step 2: Run the helper suite, clippy and rustfmt**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build --release --target x86_64-pc-windows-gnu -p uw-helper --example overlay_probe
cargo clippy -p uw-test-overlay -p uw-helper --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-test-overlay -p uw-helper --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/uw-helper/examples/overlay_probe.rs
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: both clippies clean; rustfmt prints nothing (the probe stays rustfmt-clean, as 2a left it); helper `24 passed` (about 170 s); no leftovers.

- [ ] **Step 3: Commit**

```bash
git add crates/uw-test-overlay crates/uw-helper
git commit -m "data: Wine tests for resume, a burst under a slow host, a busy info lock and a broken store"
```

- [ ] **Step 4: Check that the new tests bite, then restore**

Five mutations, each built and run on its own, then undone with `git checkout`:
1. In `crates/uw-helper/src/bin/uw-helper64/main.rs`, replace `                data::spawn(pid, data_channel, data_resume);` with `                data::spawn(pid, data_channel, None);` (`--data-resume` ignored). Run the test `a_new_helper_resumes`.
2. In `crates/uw-helper/src/bin/uw-helper64/data.rs`, replace `                    if d.lost > 0 {` with `                    if d.lost > u64::MAX {` (`data_lost` never sent). Run `a_burst_while`.
3. In `crates/uw-pipe-proto/src/data/ring.rs`, replace `        let lost = c.lost + (head.dropped - self.dropped);` with `        let lost = c.lost;` (the queue's drops not counted). Run `a_burst_while`.
4. In `crates/uw-helper/src/bin/uw-helper64/data.rs`, replace `                Err(ConsumeError::Busy) => owed.info = true,` with `                Err(ConsumeError::Busy) => return Ok(()),` (a busy info lock ends the relay as if the producer left). Run `a_busy_info`.
5. In the same file, replace the four lines

```rust
                if let Err(e) = ended {
                    report(channel, &e);
                    wait_for_disconnect(&c);
                }
```

with

```rust
                if let Err(e) = ended {
                    report(channel, &e);
                }
```

(after a read error it attaches again at once instead of waiting for `disconnect`). Run `a_store_broken`.

For each, with `<test>` the name given:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 600 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 <test>
git checkout -- crates/uw-helper/src/bin/uw-helper64/main.rs crates/uw-helper/src/bin/uw-helper64/data.rs crates/uw-pipe-proto/src/data/ring.rs
```
Then rebuild and run the four tests once more, and the leftover check:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
timeout 600 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1 a_new_helper_resumes a_burst_while a_busy_info a_store_broken
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: mutation 1 (it warns that `data_resume` is never read) fails `a_new_helper_resumes…` with `data_connected must resume at <id>:<seq>` (`left: (<id>, <a later seq>)`: the write head); mutation 2 fails `a_burst_while…` with `seq <n> after <m>: a gap of <g>, but only 0 announced`; mutation 3 fails it with `the queue's drops were not reported: lost <x>, gaps <x>`; mutation 4 fails `a_busy_info…` with `no data event within 2.99…s` (the relay ended at the first busy read, and its `data_disconnected` came before the drain; the new attach waits on the held lock, so not even the `state_seen` event arrives); mutation 5 fails `a_store_broken…` with `` expected no data event, got {"event":"data_disconnected","channel":1} ``. After the restore all four pass; `git status` is clean; no leftovers.

---

## Task 10: `uw-host` keeps the resume point and passes `--data-resume` on restart

Design "`uw-host`": per channel it keeps the producer id and the last seq: `data_connected` sets the seq to `from_seq` (a new producer id replaces the old one), each `data_event` sets it to that event's seq. Every helper it starts again gets `--data-resume <producer_id>:<seq>` (for channel 1, the one a helper reads by default). Data events carry no `request_id`, so they never touch pending commands, and they reach the user unchanged; `data_version_mismatch` and `data_bad_layout` are not stop codes. The pure supervisor builds the arguments (`Action::StartHelper { args }`); the runtime hands them to `Launch::command(args)`, and `WineLaunch` appends them to the helper's command line. The tests: the supervisor's bookkeeping on a fake clock, `--data-resume` reaching a restarted `sh` fake helper, and the design's restart scenario through `uw-inject`: kill the helper, and every tick from the downtime arrives exactly once, with contiguous seqs.

**Files:**
- Modify: `crates/uw-host/src/supervisor.rs`, `crates/uw-host/src/host.rs`, `crates/uw-host/tests/supervisor.rs`, `crates/uw-host/tests/host.rs`, `crates/uw-inject/tests/wine_e2e.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/uw-host/tests/supervisor.rs`, make these 8 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
use uw_host::supervisor::{Action, Input, Supervisor, OVERLAY_TIMEOUT_MS};
use uw_host::RequestIds;
use uw_pipe_proto::jsonl::{Command, Event};

```

with:

```rust
use uw_host::supervisor::{Action, Input, Supervisor, OVERLAY_TIMEOUT_MS};
use uw_host::RequestIds;
use uw_pipe_proto::data::{ResumePoint, SchemaCategory};
use uw_pipe_proto::jsonl::{Command, Event};

```

2. Replace:

```rust
        let ids = RequestIds::default();
        let mut sup = Supervisor::new(Desired::new(Some(OVERLAY.into())), ids.clone());
        assert_eq!(sup.start(0), vec![Action::StartHelper]);
        Fake { sup, ids, now: 0 }
    }
```

with:

```rust
        let ids = RequestIds::default();
        let mut sup = Supervisor::new(Desired::new(Some(OVERLAY.into())), ids.clone());
        assert_eq!(sup.start(0), vec![start_helper(&[])]);
        Fake { sup, ids, now: 0 }
    }
```

3. Replace:

```rust
}

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
```

with:

```rust
}

fn start_helper(args: &[&str]) -> Action {
    Action::StartHelper { args: args.iter().map(|a| a.to_string()).collect() }
}

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
```

4. Replace:

```rust
        assert_eq!(f.at(t + delay - 1), vec![], "too early for attempt {attempt}");
        t += delay;
        assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt }), Action::StartHelper]);
    }
    // This helper stays up 1 ms short of a minute: no reset, the delay stays at 10 s.
```

with:

```rust
        assert_eq!(f.at(t + delay - 1), vec![], "too early for attempt {attempt}");
        t += delay;
        assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt }), start_helper(&[])]);
    }
    // This helper stays up 1 ms short of a minute: no reset, the delay stays at 10 s.
```

5. Replace:

```rust
    assert_eq!(f.sup.next_deadline(), Some(t + 10_000));
    t += 10_000;
    assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt: 7 }), Action::StartHelper]);
    // This one stays up a minute: the next delay is the first one again.
    t += 60_000;
```

with:

```rust
    assert_eq!(f.sup.next_deadline(), Some(t + 10_000));
    t += 10_000;
    assert_eq!(f.at(t), vec![emit(Event::HelperRestarted { attempt: 7 }), start_helper(&[])]);
    // This one stays up a minute: the next delay is the first one again.
    t += 60_000;
```

6. Replace:

```rust
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 1_000));
    assert_eq!(f.at(t + 1_000), vec![emit(Event::HelperRestarted { attempt: 1 }), Action::StartHelper]);
}

```

with:

```rust
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), Some(t + 1_000));
    assert_eq!(f.at(t + 1_000), vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&[])]);
}

```

7. Replace:

```rust
        f.now = 100;
        f.exits();
        assert_eq!(f.at(1_100), vec![emit(Event::HelperRestarted { attempt: 1 }), Action::StartHelper], "{code}");
    }
}
```

with:

```rust
        f.now = 100;
        f.exits();
        assert_eq!(f.at(1_100), vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&[])], "{code}");
    }
}
```

8. Replace:

```rust
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), None);
}
```

with:

```rust
    assert_eq!(f.exits(), vec![]);
    assert_eq!(f.sup.next_deadline(), None);
}

fn data_connected(producer_id: u64, from_seq: u64) -> Event {
    Event::DataConnected {
        channel: 1,
        producer_id,
        from_seq,
        schema: vec![SchemaCategory::new("test", &[("ticks", 32)])],
    }
}

fn data_event(seq: u64) -> Event {
    Event::DataEvent { channel: 1, seq, name: "tick".into(), data: seq.to_string(), ts: seq * 1_000 }
}

/// Catches a resume point that does not follow the design (design 2b "uw-host": on
/// `data_connected` the seq becomes `from_seq` and a new producer replaces the old one; on each
/// `data_event` it becomes that event's seq), or one moved by events that carry no seq.
#[test]
fn the_resume_point_follows_data_connected_and_each_data_event() {
    let mut f = Fake::started();
    assert_eq!(f.sup.data_resume(1), None);
    assert_eq!(f.says(data_connected(77, 5)), vec![emit(data_connected(77, 5))]);
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 5 }));
    f.says(data_event(6));
    f.says(data_event(7));
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 7 }));
    f.says(Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: None, ts: 1 });
    f.says(Event::DataLost { channel: 1, events: 3 });
    f.says(Event::DataDisconnected { channel: 1 });
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 77, seq: 7 }));
    // A new producer (a new game session) replaces the old one.
    f.says(data_connected(88, 40));
    assert_eq!(f.sup.data_resume(1), Some(ResumePoint { producer_id: 88, seq: 40 }));
    assert_eq!(f.sup.data_resume(2), None);
}

/// Catches a restarted helper started without `--data-resume` (it would start at the write head
/// and lose the events of the downtime), or with a stale point.
#[test]
fn a_restarted_helper_gets_data_resume_with_the_last_seq_delivered() {
    let mut f = Fake::started();
    f.says(data_connected(77, 5));
    f.says(data_event(6));
    f.now = 100;
    f.exits();
    assert_eq!(
        f.at(1_100),
        vec![emit(Event::HelperRestarted { attempt: 1 }), start_helper(&["--data-resume", "77:6"])]
    );
    f.says(data_connected(77, 6));
    f.says(data_event(7));
    f.now = 2_000;
    f.exits();
    assert_eq!(
        f.at(4_000),
        vec![emit(Event::HelperRestarted { attempt: 2 }), start_helper(&["--data-resume", "77:7"])]
    );
}

/// Catches data events taken for replies (design 2b: they carry no request id, so they never
/// touch pending commands), changed on the way to the user, or answered with reconcile commands.
#[test]
fn data_events_pass_through_and_never_touch_pending_commands() {
    let mut f = Fake::started();
    let ping = Command::Ping { request_id: f.ids.next() };
    f.user(ping);
    for e in [
        data_connected(77, 0),
        data_event(1),
        Event::Info { channel: 1, category: "test".into(), key: "ticks".into(), value: Some("1".into()), ts: 1 },
        Event::DataLost { channel: 1, events: 2 },
        Event::DataDisconnected { channel: 1 },
    ] {
        assert_eq!(f.says(e.clone()), vec![emit(e)]);
    }
    // The ping is still pending: the helper's death answers it, once.
    assert_eq!(f.exits(), vec![emit(Event::reply_error(1, "overlay_not_connected"))]);
}

/// Catches `data_version_mismatch` or `data_bad_layout` stopping the host (design 2b: "Not stop
/// codes"; the helper reports them and waits for `disconnect`).
#[test]
fn data_errors_are_reported_and_do_not_stop_the_host() {
    for code in ["data_version_mismatch", "data_bad_layout"] {
        let mut f = Fake::started();
        let e = Event::data_error(code, 1);
        assert_eq!(f.says(e.clone()), vec![emit(e)], "{code}");
        assert!(!f.sup.is_stopped(), "{code}");
    }
}
```

In `crates/uw-host/tests/host.rs`, make these 2 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
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
```

with:

```rust
use uw_pipe_proto::jsonl::{Command as HelperCommand, Event};

/// Starts `sh -c <script> uw-helper64 <args>…` for each start, in order (the script sees the
/// host's extra arguments as `$@`); past the last one, `game_not_found`.
struct Scripts(Vec<&'static str>);

impl Launch for Scripts {
    fn command(&mut self, args: &[String]) -> Result<Command, String> {
        if self.0.is_empty() {
            return Err("game_not_found".into());
        }
        let mut c = Command::new("sh");
        c.arg("-c").arg(self.0.remove(0)).arg("uw-helper64").args(args);
        Ok(c)
    }
```

2. Replace:

```rust
    assert_eq!(next(&host), Event::Stopped { reason: "game_exited".into() });
}
```

with:

```rust
    assert_eq!(next(&host), Event::Stopped { reason: "game_exited".into() });
}

/// Catches the resume point not reaching a restarted helper's command line (design 2b
/// "uw-host": `--data-resume <producer_id>:<seq>`, the seq of the last `data_event` delivered),
/// or data events not passed to the user unchanged.
#[test]
fn a_restarted_helper_gets_data_resume_from_the_last_data_event() {
    let connected = r#"{"event":"data_connected","channel":1,"producer_id":77,"from_seq":3,"schema":[]}"#;
    let tick = r#"{"event":"data_event","channel":1,"seq":4,"name":"tick","data":"1","ts":9}"#;
    let host = Host::start(
        Scripts(vec![
            // Both lines, then die.
            r#"echo '{"event":"data_connected","channel":1,"producer_id":77,"from_seq":3,"schema":[]}'; echo '{"event":"data_event","channel":1,"seq":4,"name":"tick","data":"1","ts":9}'; exit 1"#,
            // Print the arguments it got (not an event: a protocol error), then wait for stdin.
            r#"printf 'args %s\n' "$*"; read -r line; exit 0"#,
        ]),
        Desired::new(None),
    );
    assert_eq!(next(&host), Event::parse(connected).unwrap());
    assert_eq!(next(&host), Event::parse(tick).unwrap());
    assert_eq!(next(&host), Event::HelperRestarted { attempt: 1 });
    assert_eq!(next(&host), Event::HelperProtocolError { line: "args --data-resume 77:4".into() });
}
```

In `crates/uw-inject/tests/wine_e2e.rs`, make this edit (each "replace" block is exact and occurs once):

1. Replace:

```rust
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}
```

with:

```rust
    assert_eq!(event_seqs(&seen), [from_seq + 1], "{seen:?}");
}

/// Catches a restarted helper that does not resume the data channel (design 2b "Failures":
/// `uw-host` restarts it with `--data-resume`; the new helper sends `data_connected` with
/// `from_seq` the last seq delivered, the full snapshot, then every event after it): every tick
/// from the downtime must arrive exactly once, with contiguous seqs and no `data_lost`.
#[test]
#[ignore = "needs Wine and the x86_64-pc-windows-gnu artifacts"]
fn a_killed_helper_resumes_the_data_channel_with_no_gap_and_no_duplicate() {
    let fx = start_game("dataresume");
    let mut inj = Inject::start(&fx);
    let (id, from_seq, _) = expect_connected(&mut inj, Duration::from_secs(200));
    let mut seen = data_until(&mut inj, Duration::from_secs(10), |e| {
        matches!(e, Event::DataEvent { seq, .. } if *seq == from_seq + 2)
    });

    let helper = pid_in(&fx.prefix, "uw-helper64.exe").expect("the helper runs in the prefix");
    // SAFETY: kills the test's own helper process.
    unsafe { libc::kill(helper, libc::SIGKILL) };
    // What came before `helper_restarted` is what uw-host delivered: its resume point.
    seen.extend(data_until(&mut inj, Duration::from_secs(30), |e| *e == Event::HelperRestarted { attempt: 1 }));
    let last = *event_seqs(&seen).last().unwrap();
    let (again, resumed_after, snapshot) = expect_connected(&mut inj, Duration::from_secs(60));
    assert_eq!((again, resumed_after), (id, last), "the new helper must resume after the last seq delivered");
    assert_eq!(snapshot.len(), 2, "the full snapshot: {snapshot:?}");
    // The downtime's ticks (a few seconds' worth), then on.
    seen.extend(data_until(&mut inj, Duration::from_secs(15), |e| {
        matches!(e, Event::DataEvent { seq, .. } if *seq == last + 4)
    }));
    let seqs = event_seqs(&seen);
    let want: Vec<u64> = (from_seq + 1..).take(seqs.len()).collect();
    assert_eq!(seqs, want, "a gap or a duplicate: {seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, Event::DataLost { .. })), "{seen:?}");
    assert!(pid_in(&fx.prefix, "uw-test-game.exe").is_some(), "the game stopped");
}
```

- [ ] **Step 2: Run them to see them fail**

Run:
```bash
cargo test -p uw-host --test supervisor --test host
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1 a_killed_helper_resumes
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected: the unit tests do not compile: `` error[E0559]: variant `Action::StartHelper` has no field named `args` ``, `` error[E0599]: no method named `data_resume` found for struct `Supervisor` ``, and in `host.rs` `` error[E0050]: method `command` has 2 parameters but the declaration in trait `command` has 1 ``. The Wine test (the `uw-inject` bin and that test still build) FAILS with `the new helper must resume after the last seq delivered`, `left: (<id>, <the write head>)`, `right: (<id>, <the last seq>)`; no leftovers.

- [ ] **Step 3: Keep the resume point and pass it on**

In `crates/uw-host/src/supervisor.rs`, make these 7 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
//! - `status ok` without `overlay_connected` within 10 s is `overlay_timeout`;
//! - a stdout line that is not an event is `helper_protocol_error {line}`;
//! - after every (re)connect the `Driver` reconciles the desired state.

use uw_pipe_proto::jsonl::{Command, Event};

```

with:

```rust
//! - `status ok` without `overlay_connected` within 10 s is `overlay_timeout`;
//! - a stdout line that is not an event is `helper_protocol_error {line}`;
//! - after every (re)connect the `Driver` reconciles the desired state;
//! - per data channel it keeps a resume point (design 2b "uw-host"): the producer and the last seq
//!   delivered, from `data_connected` and each `data_event`. Every new helper gets
//!   `--data-resume <producer_id>:<seq>` for the channel it reads. Data events carry no request
//!   id, so they never touch pending commands, and they reach the user unchanged.

use std::collections::BTreeMap;

use uw_pipe_proto::data::{ResumePoint, DEFAULT_CHANNEL};
use uw_pipe_proto::jsonl::{Command, Event};

```

2. Replace:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start a helper. The runtime later reports `HelperExited`, or `StartFailed` at once.
    StartHelper,
    /// Write this command to the current helper's stdin.
    Send(Command),
```

with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start a helper with these extra arguments (`--data-resume` once a resume point is held).
    /// The runtime later reports `HelperExited`, or `StartFailed` at once.
    StartHelper { args: Vec<String> },
    /// Write this command to the current helper's stdin.
    Send(Command),
```

3. Replace:

```rust
    overlay_up: bool,
    overlay_deadline: Option<u64>,
}

```

with:

```rust
    overlay_up: bool,
    overlay_deadline: Option<u64>,
    /// Per data channel: the producer and the last seq delivered.
    resume: BTreeMap<u32, ResumePoint>,
}

```

4. Replace:

```rust
            overlay_up: false,
            overlay_deadline: None,
        }
    }
```

with:

```rust
            overlay_up: false,
            overlay_deadline: None,
            resume: BTreeMap::new(),
        }
    }
```

5. Replace:

```rust
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        self.phase = Phase::Running { since: now };
        vec![Action::StartHelper]
    }

```

with:

```rust
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        self.phase = Phase::Running { since: now };
        vec![Action::StartHelper { args: self.helper_args() }]
    }

    /// The resume point of data channel `channel`, once it delivered something.
    pub fn data_resume(&self, channel: u32) -> Option<ResumePoint> {
        self.resume.get(&channel).copied()
    }

    /// The arguments for a new helper: `--data-resume` for the channel it reads (the default
    /// one; `uw-host` passes no `--data-channel`), if a resume point is held.
    fn helper_args(&self) -> Vec<String> {
        match self.data_resume(DEFAULT_CHANNEL) {
            Some(r) => vec!["--data-resume".to_string(), r.to_string()],
            None => vec![],
        }
    }

```

6. Replace:

```rust
            self.phase = Phase::Running { since: now };
            out.push(Action::Emit(Event::HelperRestarted { attempt: self.attempt }));
            out.push(Action::StartHelper);
        }
        if self.overlay_deadline.is_some_and(|d| now >= d) {
```

with:

```rust
            self.phase = Phase::Running { since: now };
            out.push(Action::Emit(Event::HelperRestarted { attempt: self.attempt }));
            out.push(Action::StartHelper { args: self.helper_args() });
        }
        if self.overlay_deadline.is_some_and(|d| now >= d) {
```

7. Replace:

```rust
            }
            Event::OverlayDisconnected => self.overlay_up = false,
            _ => {}
        }
```

with:

```rust
            }
            Event::OverlayDisconnected => self.overlay_up = false,
            // The resume point (design 2b): `from_seq` on connect (a new producer replaces the
            // old one), then each event's seq.
            Event::DataConnected { channel, producer_id, from_seq, .. } => {
                self.resume.insert(*channel, ResumePoint { producer_id: *producer_id, seq: *from_seq });
            }
            Event::DataEvent { channel, seq, .. } => {
                if let Some(r) = self.resume.get_mut(channel) {
                    r.seq = *seq;
                }
            }
            _ => {}
        }
```

In `crates/uw-host/src/host.rs`, make these 4 edits, in order (each "replace" block is exact and occurs once):

1. Replace:

```rust
/// How the host starts one helper process.
pub trait Launch: Send + 'static {
    /// The helper's command line (the host pipes its stdin and stdout), or the error code
    /// (`game_not_found`, `no_wine_session`) when the game or its Wine session is gone.
    fn command(&mut self) -> Result<Command, String>;
}

```

with:

```rust
/// How the host starts one helper process.
pub trait Launch: Send + 'static {
    /// The helper's command line with `args` after the helper's own (`--data-resume` on a
    /// restart), or the error code (`game_not_found`, `no_wine_session`) when the game or its Wine
    /// session is gone. The host pipes its stdin and stdout.
    fn command(&mut self, args: &[String]) -> Result<Command, String>;
}

```

2. Replace:

```rust

impl Launch for WineLaunch {
    fn command(&mut self) -> Result<Command, String> {
        let session =
            GameSession::find_with_proton(&self.exe, Some(self.session.pid), Some(self.session.proton.clone()))
```

with:

```rust

impl Launch for WineLaunch {
    fn command(&mut self, args: &[String]) -> Result<Command, String> {
        let session =
            GameSession::find_with_proton(&self.exe, Some(self.session.pid), Some(self.session.proton.clone()))
```

3. Replace:

```rust
                })?;
        let mut cmd = helper_command(&session, &self.helper, &self.exe, &self.bootstrap, self.fallback);
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
```

with:

```rust
                })?;
        let mut cmd = helper_command(&session, &self.helper, &self.exe, &self.bootstrap, self.fallback);
        cmd.args(args);
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
```

4. Replace:

```rust
        while i < actions.len() {
            match actions[i].clone() {
                Action::StartHelper => match launch.command().and_then(|cmd| spawn(cmd, &tx)) {
                    Ok(s) => {
                        stdin = Some(s);
```

with:

```rust
        while i < actions.len() {
            match actions[i].clone() {
                Action::StartHelper { args } => match launch.command(&args).and_then(|cmd| spawn(cmd, &tx)) {
                    Ok(s) => {
                        stdin = Some(s);
```

- [ ] **Step 4: Run the tests and clippy**

Run:
```bash
cargo test -p uw-host -p uw-inject
cargo clippy -p uw-host -p uw-inject --all-targets -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p uw-host -p uw-inject --all-targets -- -D warnings
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
```
Expected, all passing: `uw-host` `cli` 3, `discovery` 14, `driver` 4, `host` 5, `supervisor` 12; `uw-inject` bin unit tests 3; both clippies clean; `uw-inject` Wine `4 passed` (about 25 s); no leftovers.

- [ ] **Step 5: Commit**

```bash
git add crates/uw-host crates/uw-inject
git commit -m "data: uw-host keeps the resume point and passes --data-resume on restart"
```

- [ ] **Step 6: Check that the pass-through tests bite, then restore**

`data_events_pass_through_and_never_touch_pending_commands` and `data_errors_are_reported_and_do_not_stop_the_host` already held before this task (2a's catch-all arms); Step 2 failed them only because their file did not compile. Two mutations of `crates/uw-host/src/supervisor.rs`, each run on its own:
1. Replace `pub const STOP_CODES: [&str; 4] = ["already_running", "version_mismatch", "hook_failed", "bootstrap_timeout"];` with `pub const STOP_CODES: [&str; 5] = ["already_running", "version_mismatch", "hook_failed", "bootstrap_timeout", "data_bad_layout"];` (a data error stops the host).
2. Replace `        let mut out = vec![Action::Emit(event.clone())];` with `        let mut out = if matches!(event, Event::Info { .. }) { vec![] } else { vec![Action::Emit(event.clone())] };` (`info` lines swallowed).

For each:
```bash
cargo test -p uw-host --test supervisor
git checkout -- crates/uw-host/src/supervisor.rs
```
Then `cargo test -p uw-host --test supervisor` once more.
Expected: mutation 1 FAILS `data_errors_are_reported_and_do_not_stop_the_host` with `left: [Emit(Error { request_id: None, code: "data_bad_layout", detail: Some(1) }), Emit(Stopped { reason: "data_bad_layout" }), Stop]`; mutation 2 FAILS `data_events_pass_through_and_never_touch_pending_commands` with `left: []`, `right: [Emit(Info { channel: 1, category: "test", key: "ticks", … })]`; after the restore `12 passed`; `git status` is clean.

---

## Task 11: Owner live check in Deadlock (Proton)

Design "Testing" 4: "the snapshot arrives and ticks flow; edit mode on and off each give `state_seen`; after a helper kill, the ticks resume with no gap and no duplicate; the game keeps running." The controller runs one script with the owner in the Hideout, on a freshly started game, and records its output; the implementer only prepares Steps 1-2. Deadlock's Wine process is named `MainThrd`, so it is found by its command line (`pgrep -af 'S:.*deadlock\.exe'`); the escaped dot also keeps the pattern from matching a wrapping shell. One script in one shell, because the steps share `uw-inject`'s stdin (a FIFO held open on fd 3) and its pid. Run it as `bash target/uw-2b-live-check.sh` from a plain shell: `pgrep -f` also matches any wrapping shell whose own command line contains a pattern, which would fool the `== stop` check (plan 2a's planner hit this; so did this one's dry run). The planner dry-ran the script against the test game under plain Wine (paths swapped): every section matched, `back after 5 s`, `seqs: contiguous, 10 events`, `data_lost events: 0`, `helper gone`, and the restarted helper's command line carried `--data-resume <id>:<seq>`.

**Files:** `target/uw-2b-live-check.sh` (not tracked; `target/` is ignored); the result goes into the design doc in Step 4.

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
cat > target/uw-2b-live-check.sh <<'EOF'
#!/usr/bin/env bash
# Plan 2b live check: run with Deadlock in the Hideout, on a freshly started game. Each step prints
# under a "==" header.
set -u
D=$(mktemp -d)
# wait_for SECONDS PATTERN [COUNT]: until PATTERN occurs COUNT (default 1) times in the events.
wait_for() {
    timeout "$1" sh -c 'until [ "$(grep -c -- "$1" "$0")" -ge "$2" ]; do sleep 1; done' "$D/events" "$2" "${3:-1}"
}
# The seq of every data_event so far, one per line.
seqs() { grep '"event":"data_event"' "$D/events" | sed -E 's/.*"seq":([0-9]+),.*/\1/'; }
echo "== build"; grep ClientVersion "$HOME/.local/share/Steam/steamapps/common/Deadlock/game/citadel/steam.inf"
echo "== game"; pgrep -af 'S:.*deadlock\.exe'
mkfifo "$D/cmd"
exec 3<>"$D/cmd"
"$HOME/.local/share/underwolf/uw-inject" --overlay "$HOME/.local/share/underwolf/uw_test_overlay.dll" \
    <&3 >"$D/events" 2>"$D/stderr" &
INJ=$!
wait_for 120 '"event":"data_connected"' && wait_for 10 '"name":"tick"' 3
echo "== data"; grep -E '"event":"(data_connected|info)"' "$D/events" | head -n 3
echo "ticks: $(grep -c '"name":"tick"' "$D/events")"

echo '{"cmd":"set_state","edit_mode":true}' >&3; wait_for 10 '"name":"state_seen","data":"2"'
echo '{"cmd":"set_state","edit_mode":false}' >&3; wait_for 10 '"name":"state_seen","data":"3"'
echo "== edit mode"; grep '"name":"state_seen"' "$D/events" | sed -E 's/,"ts":[0-9]+//'

echo "== helper"; pgrep -aP "$INJ"
before=$(seqs | tail -n 1)
start=$(date +%s)
kill -KILL "$(pgrep -P "$INJ")"
wait_for 60 '"event":"data_connected"' 2 && wait_for 30 "\"seq\":$((before + 5)),"
echo "== back after $(( $(date +%s) - start )) s"
echo "== after the kill"; grep -E '"event":"(helper_restarted|data_connected|data_lost)"' "$D/events" | tail -n 2
echo "last seq before the kill: $before"
seqs | awk 'NR > 1 && $1 != p + 1 { bad = 1; print "not contiguous: " $1 " after " p } { p = $1 }
    END { print (bad ? "seqs: GAP OR DUPLICATE" : "seqs: contiguous, " NR " events") }'
echo "data_lost events: $(grep -c '"event":"data_lost"' "$D/events")"
echo "== game"; pgrep -af 'S:.*deadlock\.exe'

kill -TERM "$INJ"; wait "$INJ" 2>/dev/null
# The helper exits once its stdin closes; give it up to 10 s.
for _ in $(seq 10); do pgrep -f 'uw-helper64\.exe' > /dev/null || break; sleep 1; done
echo "== stop"; pgrep -af 'uw-helper64\.exe' || echo "helper gone"
exec 3>&-
echo "== game"; pgrep -af 'S:.*deadlock\.exe'
echo "== logs: $D (remove when done)"
EOF
bash -n target/uw-2b-live-check.sh && echo syntax ok
```

- [ ] **Step 3: Run it (controller, owner in the Hideout)**

**Restart the game before any run of this script.** The expected output assumes a game session that no overlay has touched: there the first reconcile applies seq 1, edit mode on seq 2 and off seq 3. In a session where the test overlay already runs, its producer has been ticking for a while and the seqs below are off; ask the owner to restart Deadlock and run the script again.

Run: `bash target/uw-2b-live-check.sh`

Expected, section by section:
- `== build`: one line, `ClientVersion=6819` at plan time. Record the value; if it changed, note it in the result (the script does not depend on it).
- `== game`: one line, `<pid> S:\…\deadlock.exe` (record the pid).
- `== data`: `data_connected` with the test schema, then the snapshot's two keys (`last_seq` may be `"1"` or `null`, depending on whether the reconcile's `state_seen` came before the helper attached):
  ```
  {"event":"data_connected","channel":1,"producer_id":…,"from_seq":…,"schema":[{"category":"test","keys":[{"key":"ticks","max_len":32},{"key":"last_seq","max_len":32}]}]}
  {"event":"info","channel":1,"category":"test","key":"ticks","value":"…","ts":…}
  {"event":"info","channel":1,"category":"test","key":"last_seq","value":…,"ts":…}
  ticks: 3
  ```
  (`ticks` may be 4: a tick can land between the wait and the count.) If no `data_connected` comes (the script then prints `== data` with nothing under it) or an `error` with `data_…` appears in the logs, STOP and report to the owner.
- `== edit mode` (an earlier `state_seen` with data `"1"` may come first):
  ```
  {"event":"data_event","channel":1,"seq":…,"name":"state_seen","data":"2"}
  {"event":"data_event","channel":1,"seq":…,"name":"state_seen","data":"3"}
  ```
- `== helper`: exactly one line, the helper (`uw-inject`'s only child process).
- `== back after N s`: a few seconds (the first restart waits 1 s).
- `== after the kill`:
  ```
  {"event":"helper_restarted","attempt":1}
  {"event":"data_connected","channel":1,"producer_id":<the same id>,"from_seq":<the last seq before the kill, or one after it>,"schema":[…]}
  last seq before the kill: <n>
  seqs: contiguous, <count> events
  data_lost events: 0
  ```
  (`from_seq` is the last seq `uw-host` delivered; the script read `<n>` a moment before the kill, so a tick in between makes it `<n> + 1`. `seqs: contiguous` is the design's "no gap and no duplicate".)
- each `== game`: the same pid as the first. **The game keeps running normally throughout.**
- `== stop`: `helper gone` (the helper exited when `uw-inject`'s exit closed its stdin).

**The owner watches the game** the whole time, in particular while the ticks flow (from `== data` on) and at the helper kill (`== helper` to `== back after`), and says whether there was any hitch, stutter or frame drop. Ask and note the answer; anything but "none" goes into the result as something unexpected.

- [ ] **Step 4: Record the result**

Add a "Live check (owner)" section at the end of `docs/superpowers/specs/2026-10-24-communication-2b-design.md` with the date, the `ClientVersion`, Proton 11.0, pass or fail, the "back after" time, the event count from `seqs:`, the owner's answer about hitches or stutter (while the ticks flowed and at the helper kill), and anything unexpected. Then remove the log directory the script printed, and:

```bash
git add docs/superpowers/specs/2026-10-24-communication-2b-design.md
git commit -m "data: record the Deadlock live-check result"
```

---

## Task 12: Done-check

The design's "Done when": all tests pass, the owner's live check passed, no code under `crates/` calls `CreateRemoteThread`, `WriteProcessMemory` or `VirtualAllocEx`, and clippy `-D warnings` is clean for the host and the Windows target. Verification only; it also runs every Wine suite, plan 2a's included.

**Files:** none.

- [ ] **Step 1: No remote-thread injection under `crates/`**

Run: `! grep -rnE 'CreateRemoteThread|WriteProcessMemory|VirtualAllocEx' crates/`
Expected: prints nothing, exit 0.

- [ ] **Step 2: All unit tests**

Run: `cargo test -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap`
Expected, all passing: `uw-pipe-proto` lib unit tests (`names`) 3, `bells` 3, `data_info` 6, `data_outbox` 5, `data_ring` 11, `data_schema` 6, `jsonl` 10, `pipe` 19, `state` 5; `uw-helper` `data_pass` 2, `decide` 30, `pending` 3; `uw-host` `cli` 3, `discovery` 14, `driver` 4, `host` 5, `supervisor` 12; `uw-inject` bin unit tests 3; `uw-bootstrap` `state` 3. Ignored Wine tests: `uw-game-channel` `wine_data` 1 and `wine_objects` 1, `uw-helper` 24, `uw-inject` 4.

- [ ] **Step 3: Clippy, host and Windows**

Run:
```bash
cargo clippy --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -p uw-pipe-proto -p uw-game-channel -p uw-helper -p uw-host -p uw-inject -p uw-bootstrap -p uw-test-overlay -p uw-test-game -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu --all-targets -p uw-game-channel -p uw-helper -p uw-test-overlay -p uw-test-game -- -D warnings -W clippy::undocumented_unsafe_blocks
cargo metadata --locked --format-version 1 > /dev/null
```
Expected: all three clippies clean (the third: every `unsafe` block has its `SAFETY` comment); `cargo metadata --locked` succeeds (the committed `Cargo.lock` is current).

- [ ] **Step 4: All Windows artifacts and every Wine suite**

Run:
```bash
cargo build --release --target x86_64-pc-windows-gnu -p uw-test-game -p uw-bootstrap -p uw-test-overlay -p uw-helper
cargo build --release --target x86_64-pc-windows-gnu -p uw-game-channel --examples
cargo build --release --target x86_64-pc-windows-gnu -p uw-helper --example overlay_probe
cargo build -p uw-inject --bin uw-inject
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_objects -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-game-channel --test wine_data -- --ignored --test-threads=1
timeout 900 xvfb-run -a cargo test -p uw-helper --test wine_e2e -- --ignored --test-threads=1
timeout 600 xvfb-run -a cargo test -p uw-inject --test wine_e2e -- --ignored --test-threads=1
ps -eo pid,args | grep -E 'uw-test-game|uw-helper64|_probe' | grep -vF grep; find /tmp -maxdepth 1 -name 'uw-inject-*'
git status --short
```
Expected: 2a's gate `1 passed`, the data gate `1 passed`, helper `24 passed` (about 170 s), `uw-inject` `4 passed` (about 25 s); the leftover check prints nothing; the tree is clean.

- [ ] **Step 5: The live check passed**

Confirm Task 11's result is recorded in the design doc as a pass. With Steps 1-5 green, plan 2b is done. Nothing to commit.

---

## Self-review (completed by the plan author)

**1. Design coverage.** Every requirement maps to a task:
- Scope "In": the layouts (Tasks 1-4), the Win32 objects (Task 5), the helper's data thread with attach, snapshot, deltas, events, resume and the JSON events (Tasks 6, 7), `uw-host`'s resume point and `--data-resume` (Task 10), the test producer (Tasks 8, 9) and `--bad-data-channel` (Task 7).
- Flow step 1, the create order with the hello block last and then `connect` (Task 5 `Producer::create`); step 2, a try every second, `connect` set (the gate's second consumer is refused once it is reset), the rest opened, the hello block's magic, version and pid, read-only views and layout checks, `data_connected` with the schema and `from_seq` (Tasks 1, 2, 3, 5, 7); step 3, the snapshot under one info lock with nulls, deltas from exactly that snapshot (Tasks 2, 5, 7); step 4, the start point under the event lock (Tasks 3, 5); step 5, the wait on `[disconnect, info-bell, events-bell]` with 1 s and both regions read on a timeout, 100 ms lock waits then skip, a skipped read owed to the next pass (Tasks 5, 7, 9); step 6, `data_disconnected`, close, back to step 2 (Tasks 7, 8).
- Threads: the producer on one thread of its own with `&mut self` and 0 ms tries (Tasks 4, 5, 8); the data thread as the helper's only reader, sharing only `emit()` (Task 7; lines whole under a slow host: Task 9).
- Messages and layouts: the names with one suffix per object (Task 1, uniqueness test with 2a's names); the hello block (Task 1); the info store, its schema form, the timestamp and busy-lock rules, the limits (Tasks 1, 2, 4); the ring (Task 3); text as UTF-8 and `ts` from `QueryPerformanceCounter` (Tasks 2-5); the producer API table and "errors, never silent drops" (Tasks 4, 5); the JSON events (Task 6); the helper arguments and exit 2 (Task 7); `uw-host` (Task 10).
- Failures, row by row, with the test that fails when the row breaks:
  - No producer yet: `the_data_thread_waits_for_the_producer_then_relays_it` (Task 8).
  - Helper killed or restarted: `a_killed_helper_resumes_the_data_channel_with_no_gap_and_no_duplicate` (Task 10), `a_new_helper_resumes_after_the_given_seq_only_for_the_same_producer` (Task 9), `a_restarted_helper_gets_data_resume_*` (Task 10, unit and fake helper).
  - Resume seq older than the oldest record: `a_resumed_reader_delivers_each_event_after_the_resume_seq_once`, `catch_up_gives_the_records_to_deliver_and_the_gap` (Task 3), and the gate's `attach <id>:2` (Task 5).
  - Resume names another producer: `start_after_resumes_only_the_same_producer` (Task 3) and the third helper in Task 9's resume test.
  - Producer's `dropped` grew: `growth_in_dropped_is_lost_too` (Task 3), `queue_overflow_is_counted_and_the_rest_wait_for_the_throttle` (Task 4), `a_burst_while_the_host_reads_slowly_loses_nothing_unannounced` (Task 9, mutation 3).
  - Lock busy over 100 ms: the gate's `hold-info` step (Task 5), `a_busy_info_lock_is_skipped_and_the_newest_value_comes_after` (Task 9, mutation 4; no tick phase can fail it), and the owed read's `a_read_a_busy_lock_skipped_is_retried_on_the_next_wake_of_any_kind` (Task 7, its mutation).
  - `WAIT_ABANDONED`: the gate's `abandon-events` and `abandon-info` steps (Task 5, Step 8's mutation).
  - Version not 1: `a_hello_block_that_does_not_check_out_is_refused`, the info and ring version checks (Tasks 1-3), `a_data_channel_of_another_version_is_data_version_mismatch_once` (Task 7).
  - Bad layout: Tasks 1-3's refusal tests (magic, size, offset, schema, slot length, UTF-8, seqs and counts going backwards), the gate's broken header (Task 5), `a_data_channel_with_a_bad_layout_is_data_bad_layout_once` at attach (Task 7), and `a_store_broken_after_data_connected_is_reported_once_until_disconnect` after attach (Task 9, mutation 5).
  - Game exits: `killing_the_game_reports_game_exited`, now with the data thread attached (Task 8).
  - Stdin closes: `closing_stdin_ends_the_helper_after_it_connected`, now with the data thread attached (Task 8), and 2a's window-wait test.
  - Host slow to read: `a_burst_while_the_host_reads_slowly_loses_nothing_unannounced` (Task 9).
  - The whole Linux client restarts: the second client in `uw_inject_relays_the_test_producer_and_a_new_client_starts_at_the_write_head` (Task 8).
  - Not stop codes: `data_errors_are_reported_and_do_not_stop_the_host` (Task 10, Step 6's mutation 1); data events reach the user unchanged: `data_events_pass_through_and_never_touch_pending_commands` (mutation 2).
- Differences from Spike 3: names by pid and channel (Task 1); `producer_id` (Tasks 1, 5); one throttle (Task 4); the schema parsed once (`Outbox::new` at create, Tasks 4, 5); errors to the caller and counted drops (Task 4); each pending key's own timestamp (Task 4); resume after the last delivered seq instead of replay (Tasks 3, 7, 10); JSON lines from a thread of 2a's helper (Task 7); 100 ms lock waits (Task 5); `uw-host` supervising its own helper (2a, plus Task 10).
- Platform facts: `connect` seen late (the gate's consumer opens after the producer set it) and a reset `connect` refused (the gate's second consumer), the smallest index winning (gate, with `create()`'s bells taken first), no handle closed during a wait (the data thread drops its `Consumer` only after its last wait), read-only views (`FILE_MAP_READ`), paging-file zeros (the zeroed-mapping refusal tests).
- Testing 1 (Tasks 1-4, 6, 10), 2 (Task 5, the gate), 3 (Tasks 7-10), 4 (Task 11); "Done when" (Task 12). Risks need no code; "a producer can't be re-created right away" is checked by the gate's `recreate` (`exists` until the consumer closes).

**2. Placeholder scan.** No TBD/TODO, no "similar to Task N", no step without its code; every run step names its command and expected result. New files are given whole; changes to existing files are exact replace blocks, each occurring once in the file as the previous step left it (generated from, and checked against, the scratch commits, and replayed byte for byte onto a fresh clone).

**3. Type and name consistency.** `DataObject`/`data_name` (Task 1) are what the producer, the consumer, the test game and `overlay_probe` use; `ReadError` (`Version`/`Layout`, `code()`), `Hello`, `read_hello`/`write_hello`, `Schema`/`SchemaCategory`/`SchemaKey`/`SchemaError` (Task 1); `InfoLayout`/`info_init`/`info_set`/`InfoReader`/`InfoValue` (Task 2); `ResumePoint`/`start_after`/`catch_up`/`CatchUp`, `RingReader`/`Drained`/`RingEvent`/`read_ring_head`/`ring_init`/`ring_push`/`ring_set_dropped`, `RING_SLOTS`/`RING_SIZE` (Task 3); `Outbox`/`Throttle`/`ProduceError`/`QUEUE_CAP`/`THROTTLE_MAX`/`THROTTLE_WINDOW_MS` (Task 4); `Producer`/`CreateError`/`Consumer`/`OpenError`/`ConsumeError`/`Wake`/`Attached`/`Win32Error` (Task 5) match the probes, the helper's `data.rs` and the test overlay; `Woke`/`Owed` (Task 7) match `data.rs`'s relay and Task 9's mutation 4; `Event::{DataConnected, Info, DataEvent, DataLost, DataDisconnected}` and `Event::data_error` (Task 6) match the helper, `uw-host` and every test; `Action::StartHelper { args }`, `Supervisor::data_resume`, `Launch::command(args)` (Task 10) match the runtime and its tests; the test schema (`test`: `ticks` 32, `last_seq` 32) and the env variables (`UW_TEST_OVERLAY_DATA_RESTART_MS`, `UW_TEST_OVERLAY_BURST`) match the tests and the live check.

**Not in this plan, by the design:** the Deadlock collector port and its fixes, plugin loading and the provider's Overwolf shapes (plan 5); more than one data channel per game; resuming across a restart of the whole Linux client.
