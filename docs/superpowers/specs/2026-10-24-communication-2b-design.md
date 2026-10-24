# Milestone 1, plan 2b: Communication, game-data channel (design)

Date: 2026-10-24. Status: implemented; the owner's live check passed (see "Live check (owner)").
Implements spec §6.4 "Data channel (copy Overwolf)" and the relay half of §6.3
(`2026-10-20-underwolf-design.md`). Proposed in `docs/spikes/SUMMARY.md`, milestone 1 item 2: "the
Spike 3 data-channel relay folds into the same helper". Builds on plan 2a
(`2026-10-23-communication-2a-design.md`).

Owner decisions (2026-10-23):
- **Scope:** the channel and its relay, proved with a test producer. The collector port stays in
  plan 5.
- **Who creates the objects:** the producer, as in Spike 3 and the spec.
- **After a helper restart:** event delivery resumes after the last event `uw-host` delivered.
- **Where the helper reads the channel:** on its own data thread, outside 2a's relay loop.

## Goal

An in-game producer publishes info keys and events through named shared memory. The Wine-side
helper reads them and relays them to the Linux side, where `uw-host` passes them on, as spec §6.4
describes. Plan 5's Deadlock provider builds on it: its collector becomes a producer, and its
provider consumes the events `uw-host` passes on.

## Scope

In:
- **Layouts:** the hello block, the info store with its schema, and the event ring
  (`uw-pipe-proto::data`, pure code).
- **Win32 objects:** the producer API and the consumer (`uw-game-channel::data`).
- **The helper's data thread:** attach, snapshot, deltas, events, resume, and the new JSON events.
- **`uw-host`:** the resume point, and `--data-resume` on restart.
- **Test changes:** the test overlay as a test producer, and a `--bad-data-channel` test game flag.

Out:
- The Deadlock collector port and its fixes (`tower_destroyed`, the schema), plugin loading, and
  the provider's Overwolf shapes: plan 5.
- More than one data channel per game. Milestone 1 uses channel 1; the names and events carry the
  channel id, so more can come later.
- Resuming across a restart of the whole Linux client (see "Failures").

## Components and flow

| Piece | Where | Role |
|---|---|---|
| Layouts | `uw-pipe-proto::data` (new) | Hello block, info store and schema, event ring, resume arithmetic. Byte-slice code, unit-tested on Linux. |
| Producer | `uw-game-channel::data` (new) | Creates every object, owns the schema, writes info and events from the producer's own thread. |
| Consumer | `uw-game-channel::data` | Opens the objects read-only and checks every read. Used only by the helper. |
| Data thread | `uw-helper` (new `data.rs`) | Opens the channel, emits `data_connected`, the snapshot, deltas and events, and resumes after a restart. |
| Host | `uw-host` | Passes the data events on, remembers the resume point, and gives it to each new helper. |
| Test producer | `uw-test-overlay` | A small test schema, a `tick` event every second, and a `state_seen` event after each `state_applied`. |

1. **The producer creates the channel.** `Producer::create(channel, schema)` creates every object
   in this order:
   - the hello block;
   - the info store, with its schema written in;
   - the event ring;
   - both locks;
   - the two signals;
   - `disconnect`, then `connect`.

   It writes the hello block last, then sets `connect`. The schema is parsed once, here.
2. **The helper attaches.** The helper starts its data thread when its relay starts. Every 1 s
   the thread tries to open `connect` and checks that it is signalled. Once it is, the thread:
   - opens the rest;
   - checks the hello block's magic and version, and that its pid is the game's pid;
   - maps both regions read-only and checks their layouts;
   - emits `data_connected` with the schema and `from_seq`.
3. **The snapshot.** Under one info lock, the thread reads every key with a non-zero timestamp
   and emits one `info` line per key, with nulls included. Later deltas continue from exactly
   that snapshot.
4. **Where events start.** Under the event lock, the thread picks its start point. If
   `--data-resume` names this producer, it starts there; otherwise at the write head.
5. **The wait loop.** The thread waits on `[disconnect, info-bell, events-bell]` with a 1 s
   timeout. `disconnect` has the smallest index, so it wins a tie.
   - On `info-bell`: emit the keys whose timestamps moved.
   - On `events-bell`: drain the ring in seq order.
   - On a timeout: read both regions anyway, because a signal can be missed while a lock attempt
     times out.

   Lock waits are at most 100 ms; on a timeout the thread skips and retries on the next pass.
6. **Disconnect.** On `disconnect` the thread first reads both regions once more, because the
   producer may have written just before it went while the thread was busy (for example, blocked
   on a full stdout), and `disconnect` wins the tie with the bells. The views stay valid while the
   thread holds the handles. It emits what it finds as on any pass (`info`, `data_lost`,
   `data_event`); a lock still busy after 100 ms skips that read. Then it emits
   `data_disconnected`, closes everything and goes back to step 2.

Threads:
- **The producer** calls `commit`, `push_event` and `flush` from one thread of its own (the API
  takes `&mut self`). It never waits on a shared lock; it only tries with 0 ms. A busy lock leaves
  the work pending for the next call. In 2b the producer runs on a thread of its own in the test
  overlay, not on 2a's channel worker.
- **The data thread is the helper's only reader.** It shares only `emit()` with 2a's relay loop.
  `emit()` writes each line whole under one lock, so control and data lines never interleave
  inside a line.

## Messages and layouts

**Names:** `Local\underwolf-<pid>-data-<channel>-<suffix>`, where `<pid>` is the game's Wine pid
and `<channel>` is a u32. A suffix per kind (objects of every kind share one namespace):

| Suffix | Kind |
|---|---|
| `hello`, `info`, `events` | file mappings (paging-file backed) |
| `info-lock`, `events-lock` | mutexes |
| `info-bell`, `events-bell` | auto-reset events |
| `connect`, `disconnect` | manual-reset events |

The names go in `uw-pipe-proto::names`, next to 2a's.

**Hello block, 24 bytes, little-endian:**

| Offset | Field |
|---|---|
| 0 | magic `UWDH` |
| 4 | format version u32 (1) |
| 8 | producer pid u32 |
| 12 | reserved u32 |
| 16 | `producer_id` u64 |

`producer_id` is the producer's `GetTickCount64()` at creation. With the pid, it tells one game
session from the next.

**Info store:** Spike 3's layout, with magic `UWDI` and version 1. The header is 32 bytes:
`schema_len u32 @8`, `key_count u32 @12`, `slot_area_off u32 @16`, with bytes 20 to 31 reserved.
Spike 3's unused `commit_seq` is dropped. After the header come:
1. **The schema.** A u32 category count. For each category: `u16 len, name, u32 key_count`. For
   each key: `u16 len, name, u32 max_len`. Keys are numbered across categories in order.
2. **Timestamps.** One u64 per key, 8-aligned. 0 means never set.
3. **Slots.** One per key: `len u32` plus `max_len` bytes, padded to 8. `len = u32::MAX` means
   null.

The rules:
- **Timestamps:** a key's timestamp only moves forward (`max(ts, prev + 1)`), and an unchanged
  value keeps its slot and its timestamp.
- **A busy lock:** the producer keeps the newest value per key, each with its own timestamp, and
  writes them on the next call.
- **Limits:** names of at most 255 bytes, `max_len` of at most 64 KiB, at most 1024 keys, at most 1024 categories.

**Event ring:** a header, then 30 records.
- **The header:** magic `UWDE`, version u32 (1), `write_seq u64 @8` (the last seq written), and
  `dropped u64 @16` (events the producer's queue dropped).
- **Each record:** `seq u64`, `ts u64`, `name_len u16` + 255 name bytes, and `data_len u32` +
  63 KiB of data, as in Spike 3.
- **Placement:** seqs start at 1, and a record lives in slot `seq % 30`.

**Text:** values, event names and event data are UTF-8, because the producer API takes `&str`.
`ts` is the producer's clock in ms (`QueryPerformanceCounter`).

**Producer API (`uw-game-channel::data::Producer`):**

| Call | Does |
|---|---|
| `create(channel, schema)` | Creates the channel. If any object already exists, it fails with `Exists`. |
| `commit(&[(category, key, Option<&str>)])` | One info transaction per pass. Signals `info-bell` once if anything changed. |
| `push_event(name, data)` | Queues an event: a FIFO of 256, oldest dropped and counted in `dropped`. One throttle of 30 events per 200 ms; events over the budget wait in the queue. |
| `flush()` | Writes pending info and queued events, within the throttle. Call it regularly (the test producer does every 100 ms). |
| `Drop` | Resets `connect`, then sets `disconnect`. |

An unknown key, or a value, name or data over its limit, comes back as an error. Nothing is
silently dropped or truncated.

**New JSON events, helper → host.** Each carries `channel`.

| Event | Fields | When |
|---|---|---|
| `data_connected` | `producer_id`, `from_seq`, `schema`: `[{category, keys: [{key, max_len}]}]` | The channel was opened and checked. `from_seq` is the seq after which events will be delivered. |
| `info` | `category`, `key`, `value` (string or null), `ts` | The snapshot, then each changed key. |
| `data_event` | `seq`, `name`, `data`, `ts` | Each event, in seq order. |
| `data_lost` | `events` | Events overwritten before they were read, plus growth in the producer's `dropped`. Sent before the events that follow the gap. |
| `data_disconnected` | (channel only) | The producer set `disconnect`. |
| `error` `data_version_mismatch` / `data_bad_layout` | `detail`: the channel id | See "Failures". Not stop codes. |

**Helper arguments:** `--data-channel <n>` (default 1), and `--data-resume <producer_id>:<seq>`.
A malformed value is a usage error, exit 2, like the helper's other arguments.

**`uw-host`:**
- **The resume point.** Per channel it keeps the `producer_id` and the last seq. On
  `data_connected` the seq becomes `from_seq` (and the producer id is replaced if it differs). On
  each `data_event` it becomes that event's seq.
- **Restarts.** When it restarts the helper and holds a resume point, it passes
  `--data-resume <producer_id>:<seq>`.
- **Not replies.** Data events carry no `request_id`, so they never touch pending commands.
  `uw-host` passes them to its user unchanged.

## Failures

| Case | What happens |
|---|---|
| No producer yet | The data thread retries once a second and emits nothing until the channel opens. |
| Helper killed or restarted | `uw-host` restarts it with 2a's backoff and passes `--data-resume`. The new helper sends `data_connected` (`from_seq` = the resume seq), the full snapshot, then every event after the resume seq. |
| Resume seq older than the ring's oldest record | `data_lost {events: <the gap>}`, then delivery from the oldest record. No event is delivered twice. |
| Resume names another producer (a new game, a reused pid) | Start at the write head, as on a first attach. |
| Producer's `dropped` grew | The difference goes into `data_lost`. The baseline is the count when the helper attaches, so drops while no helper runs are not reported. |
| Lock busy for more than 100 ms | Skip and retry on the next pass. |
| `WAIT_ABANDONED` on a lock | Taken as acquired, then the headers are re-checked (2a's rule). |
| Hello or header version not 1 | `error data_version_mismatch` (detail: the channel id), once per producer. The thread stops reading and waits for `disconnect`. |
| Bad layout: a magic, size, offset or schema that doesn't fit; a slot length over its `max_len`; text that isn't UTF-8; a seq that goes backwards under the same producer id | `error data_bad_layout`, once per producer, and the same wait for `disconnect`. Every read is checked: the spec's "stop rather than emit wrong data". |
| Game exits | `game_exited` stops everything, as in 2a. |
| Stdin closes | The helper exits, as in 2a, in every phase, including while the data thread runs. |
| A panic in the data thread | The helper prints a line on stderr and exits with code 3, so the relay does not run on without its data. `uw-host` restarts it, as for any helper exit, with `--data-resume`. |
| Host slow to read (stdout full) | `emit()` blocks the writer, as in 2a. Lines are never split or reordered. |
| The whole Linux client restarts | `uw-host`'s resume point is lost, so the new helper starts at the write head. Events from the client's downtime are not delivered; the snapshot still restores every current value. |

## Differences from Spike 3

| Spike 3 | 2b | Why |
|---|---|---|
| Names keyed by channel only | Names keyed by pid and channel | As 2a: two games in one Wine session can't clash. |
| Hello block: version and pid | Adds `producer_id` | So a resume can't attach to a new game that reused the pid. |
| Two event throttles | One | The C3b ruling; a refusal by the second throttle spent the first one's budget again. |
| Schema re-parsed on every flush | Parsed once, at create | The spike did this work on the game thread. |
| Some writes silently dropped | Errors go to the caller; queue drops are counted and relayed | §6.4 fail safe. |
| Merged pending keys share one timestamp | Each pending key keeps its own | A key's timestamp should say when it was set. |
| Replay the last 30 events, de-duplicate on Linux by `(name, ts)` | Resume after the last delivered seq | The ruling "start at the write head or mark replays"; de-duplication also merged real events. |
| TCP and postcard, a second helper process | JSON lines on 2a's helper stdout, from a thread | SUMMARY item 2: one helper. |
| A 2 s consumer lock wait | 100 ms, then retry | As 2a. |
| `uw-gep` killed every helper the user ran at start | `uw-host` supervises its own helper | 2a. |

Overwolf: the channel's shape (connect and disconnect events, a version and pid block, an info
store with a fixed schema and per-key timestamps, a ring of about 30 records, and a throttle of
about 30 events per short window) is spec §6.4's description of Overwolf's. Its byte layout is
ours, from Spike 3. Overwolf's client reads the shared memory directly. The relay, and with it the
resume point, has no Overwolf equivalent (spec §6.3).

## Platform facts the design relies on

Checked 2026-10-24 against Microsoft Learn. Context7's Win32 index returned only stubs for these
pages, so the pages were read directly.

- **`CreateEventW`, `bManualReset = TRUE`:**
  - "requires the use of the ResetEvent function to set the event state to nonsignaled".
  - "it remains signaled until it is explicitly reset … Any number of waiting threads, or threads
    that subsequently begin wait operations … can be released while the object's state is
    signaled". So a helper that opens `connect` late still sees it set.
  - For an existing name, "the bManualReset and bInitialState parameters are ignored" and
    `GetLastError` returns `ERROR_ALREADY_EXISTS`.
  - Auto-reset events and the shared namespace are as in 2a.
- **`WaitForMultipleObjects`, `bWaitAll = FALSE`:**
  - "If more than one object became signaled during the call, this is the array index of the
    signaled object with the smallest index value". So `disconnect` at index 0 wins.
  - "If one of these handles is closed while the wait is still pending, the function's behavior
    is undefined". So the data thread closes its handles only after its wait returns.
  - `WAIT_ABANDONED_0 + i` grants ownership: "you should check it for consistency".
- **`MapViewOfFile` with `FILE_MAP_READ`:** "A read-only view … An attempt to write to the file
  view results in an access violation". So the helper can't corrupt the producer's data.
  - Views are coherent "for views that are mapped by different processes".
  - Paging-file pages start as 0.
- **`GetTickCount64`:** "the number of milliseconds that have elapsed since the system was
  started"; the resolution is "typically in the range of 10 milliseconds to 16 milliseconds".
  That is enough for a per-producer id, since each process has at most one producer per channel.
- **Wine:** named shared memory, events and mutexes shared between a Wine-side helper and the
  game are proven by Spike 3's channel, and the pid-named objects by 2a.
- **Bindings, `windows-sys` 0.61.2 (crate source):**
  - `GetTickCount64` is in `Win32_System_SystemInformation`.
  - `QueryPerformanceCounter` is in `Win32_System_Performance`.
  - `CreateFileMappingW`, `OpenFileMappingW` and `MapViewOfFile` are in `Win32_System_Memory`.
  - `CreateEventW`, `OpenEventW`, `CreateMutexW` and `OpenMutexW` are in `Win32_System_Threading`.

## Code layout

| Path | Contents |
|---|---|
| `crates/uw-pipe-proto/src/` | new `data.rs`: hello, schema, info store, ring, and a pure resume function. `names.rs` gains the data names; `jsonl.rs` gains the data events. |
| `crates/uw-game-channel/src/` | new `data.rs`: `Producer` (create, commit, push_event, flush, Drop) and `Consumer` (open, snapshot, changes, drain). |
| `crates/uw-helper/src/bin/uw-helper64/` | new `data.rs`: the data thread. `main.rs` parses `--data-channel` and `--data-resume` and starts the thread with the relay. |
| `crates/uw-host/` | the resume point and the restart argument |
| `crates/uw-test-overlay/` | the test producer |
| `crates/uw-test-game/` | `--bad-data-channel <version\|layout>`: a hello block with format version 2 (`data_version_mismatch`), or with a wrong magic (`data_bad_layout`) |

## Testing

1. **Unit tests (Linux):**
   - layout and schema round-trips;
   - info rules: timestamps only move forward, an unchanged value keeps its timestamp, null
     differs from empty, an unknown key and a too-long value are errors, and pending keys keep
     their own timestamps;
   - the ring: push and drain, wraparound giving a lost count, and a seq going backwards giving a
     bad layout;
   - the pure resume function: ring state plus a resume point gives the events to deliver and the
     lost count;
   - the throttle, and queue overflow counted in `dropped`;
   - `uw-host`'s resume bookkeeping, and `--data-resume` on restart (with a fake helper).
2. **The Wine gate:** a producer probe and a consumer probe in two processes. It covers the
   snapshot, deltas, events in order, ring wraparound giving `data_lost`, and an abandoned lock
   with the header re-check.
3. **Wine end-to-end tests** with the test overlay as producer. Its schema is category `test`,
   keys `ticks` and `last_seq`; it sends a `tick` event every 1 s and a `state_seen` event after
   each `state_applied`. The tests:
   - `uw-inject` sees `data_connected`, the snapshot, `info` and `data_event`;
   - `set_state` gives `state_seen`;
   - kill the helper: after the restart, every tick from the downtime arrives exactly once, with
     contiguous seqs;
   - a burst env var gives `data_lost`;
   - `--bad-data-channel version` gives `data_version_mismatch`, and `--bad-data-channel layout`
     gives `data_bad_layout`;
   - stdin closing still ends the helper while its data thread runs;
   - 2a's suites stay green.
4. **The owner's live check in Deadlock**, with the same test overlay:
   - the snapshot arrives and ticks flow;
   - edit mode on and off each give `state_seen`;
   - after a helper kill, the ticks resume with no gap and no duplicate;
   - the game keeps running.

## Done when

All tests above pass, the owner's live check passes, no code under `crates/` calls
`CreateRemoteThread`, `WriteProcessMemory` or `VirtualAllocEx`, and clippy `-D warnings` is clean
for the host and the Windows target.

## Risks

- **The ring is 1.85 MiB per channel** (30 records of up to 63 KiB, as in Spike 3). That is fine
  for one channel; reconsider if many channels come.
- **The resume point lives in `uw-host`'s memory.** A whole-client restart starts at the write
  head (see "Failures").
- **A producer can't be re-created right away.** Re-creating a producer in the same process fails
  with `Exists` while the helper still holds the old objects, which lasts until it sees
  `disconnect`. In milestone 1, producers live for the whole game session.
- **Only the test producer runs in 2b.** The collector's real load (37 keys, about 44 KiB of info,
  one pass every 50 ms) is first measured in plan 5.
- **The data thread shares stdout with 2a's relay.** A big snapshot delays control lines by the
  time it takes to write it, not by a lock wait.
- **Proton:** only Proton 11.0 has been tested.

## Live check (owner)

2026-10-24, Deadlock ClientVersion 6819, Proton 11.0, Hideout, a fresh game session. **Pass.**

- Snapshot: `data_connected` on channel 1 with the test schema (`ticks`, `last_seq`, 32 bytes
  each) and `from_seq` 1, then `info` for `ticks` (`"0"`) and `last_seq` (`"1"`, the reconcile's
  `state_seen` came before the attach). Ticks flowed: 3 within the wait.
- Edit mode on, then off: `state_seen` with data `"2"` (seq 5) and `"3"` (seq 6).
- Helper killed (`kill -KILL`): back after 5 s. `helper_restarted` attempt 1, then
  `data_connected` with the same `producer_id` and `from_seq` 6, the last seq before the kill.
  The seqs were contiguous over 10 events (no gap, no duplicate), with no `data_lost`. No `error`
  event. The new helper skipped the hook (marker present).
- Stop: SIGTERM to `uw-inject`; the helper exited because its stdin closed.
- The game process (pid 2903110) kept running throughout. The owner saw no hitch, stutter or
  frame drop, while the ticks flowed or at the helper kill.
