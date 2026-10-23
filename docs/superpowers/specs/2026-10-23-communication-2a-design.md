# Milestone 1, plan 2a: Communication, control channel (design)

Date: 2026-10-23. Status: for owner review.
Implements spec §6.3 "Client ↔ in-game channel" (`2026-10-20-underwolf-design.md`), the control
half. Proposed in `docs/spikes/SUMMARY.md`, milestone 1 item 2. Builds on plan 1
(`2026-10-22-injection-design.md`).

Owner decisions (2026-10-23): plan 2 is split into 2a (this: control channel and the Linux
library) and 2b (game-data channel relay); the overlay gets its own pipe (option 1, as Overwolf's
`OWClient` connects separately from its bootstrap); the differences from Overwolf listed below are
accepted.

Owner decisions after the plan review (2026-10-23): a `ping` the overlay does not answer within
15 s is answered `overlay_not_responding`; `hook_failed` and `bootstrap_timeout` stop `uw-host`
(no restart), like `already_running` and `version_mismatch`.

## Goal

The Linux side and the in-game overlay exchange commands, doorbells and shared state through
Windows IPC that the Wine-side helper owns and relays, the way spec §6.3 describes. Plan 3's input
layers build on it: their edit-mode, visibility and (later) hotkey and widget data arrive this way.

## Scope

In:
- The overlay pipe, two doorbell events with their mask region, and the state block (layout v1).
- `uw-game-channel`: the in-game library an overlay module uses for all of it.
- The helper serving both pipes and the objects, and relaying them over JSON lines.
- `uw-host`: a Linux library that finds the game, starts and restarts the helper, and keeps the
  overlay at a desired state. `uw-inject` becomes a thin CLI on it.
- Plan 1's deferred items (see "Plan 1 items").
- Test overlay and test game changes for automated Wine tests.

Out:
- The game-data channel (named shared memory info store and event ring) and its relay: plan 2b.
- Hotkey table and widget list in the state block (layout v2), input routing: plan 3.
- A native Windows host (no relay): later.

## Components and flow

| Crate | Change | Role |
|---|---|---|
| `uw-pipe-proto` | extended | Overlay pipe messages, object names, doorbell bits, state block layout v1, JSON commands and events. Pure Rust, unit-tested on Linux. |
| `uw-game-channel` | new, Windows | In-game library. A worker thread connects to the overlay pipe (retry 1/s, reconnect on loss), sends `overlay_hello`, opens the doorbells and the state block, delivers bells to a callback, answers `ping`. |
| `uw-helper` | extended | Creates the doorbells, mask region and state block, then the overlay pipe, before it hooks; serves both pipes; relays everything over JSON lines. |
| `uw-host` | new, Linux library | Discovery and the helper launch (moved from `uw-inject`), request ids, helper restarts, and reconciling the desired state. |
| `uw-inject` | reduced | CLI on `uw-host`: JSON commands on stdin (request ids optional, assigned if absent), events on stdout. |
| `uw-test-overlay` | extended | Uses `uw-game-channel`; applies state and rings `state_applied`. |
| `uw-test-game` | extended | `--bad-state-block`: creates the state block with a wrong header, for the mismatch test. |

Flow:
1. `uw-host` finds the game and starts the helper as in plan 1.
2. The helper creates (in order) the doorbell events, the mask region, the state block, the
   overlay pipe, then the bootstrap pipe, and hooks. The bootstrap loads the overlay (plan 1).
3. The overlay's worker connects to the overlay pipe, sends `overlay_hello` (module version,
   state layout), then opens the doorbells and the state block. The helper emits
   `overlay_connected`.
4. Host → game: `set_state` from Linux. The helper takes the state lock, writes the changed
   fields, bumps `seq`, releases, ORs the bits for the changed fields into the to-game mask and
   signals `bell-game`. The overlay's worker wakes, swaps the mask to 0, reads the block under the
   lock and hands the bits and values to the overlay.
5. Game → host: the overlay ORs bits into the to-host mask and signals `bell-host`. The helper
   swaps the mask to 0 and emits `bell`. In 2a the overlay writes `applied_seq` and rings
   `state_applied` after applying a change.
6. `ping` from Linux travels to the overlay as a pipe message; the overlay answers `pong` with the
   last `seq` it applied.

Threads: the helper keeps one stdin reader thread and the game watcher (plan 1) and waits on its
pipes and `bell-host` in one loop. In the game, only `uw-game-channel`'s worker waits on events
or the state lock. Hooks on game threads (plan 3) may ring (lock-free) and may try the state lock
with a 0 ms wait only.

`uw-host` keeps a desired state (overlay path, `edit_mode`, `visible`) and reconciles it after
every (re)connect: `load_overlay` if `bootstrap_connected` says none is loaded, a full `set_state`
on every `overlay_connected`.

## Messages and layouts

All names carry the game's Wine pid. Kernel object names share one namespace per session, so each
kind gets its own suffix.

| Object | Name | Created by |
|---|---|---|
| Overlay pipe | `\\.\pipe\underwolf-<pid>-overlay`: `PIPE_ACCESS_DUPLEX \| FILE_FLAG_FIRST_PIPE_INSTANCE \| FILE_FLAG_OVERLAPPED`, message mode, `PIPE_REJECT_REMOTE_CLIENTS`, 1 instance (as the bootstrap pipe) | helper |
| Host → game doorbell | `Local\underwolf-<pid>-bell-game`, auto-reset event | helper |
| Game → host doorbell | `Local\underwolf-<pid>-bell-host`, auto-reset event | helper |
| Mask region | `Local\underwolf-<pid>-bells`, 16 bytes: `to_game` u64 at 0, `to_host` u64 at 8 | helper |
| State block | `Local\underwolf-<pid>-state` | helper |
| State lock | `Local\underwolf-<pid>-state-lock`, mutex | helper |

The game side only opens these objects (it never creates them), after its `overlay_hello`.

**Doorbell bits v1.** Senders OR bits with an atomic operation, receivers swap the mask to 0. An
overlay ignores bits it doesn't know, so later plans add bits without a version change.
- `to_game`: bit 0 `edit_mode_on`, bit 1 `edit_mode_off`, bit 2 `show`, bit 3 `hide`.
- `to_host`: bit 0 `state_applied`.

**State block layout v1** (`#[repr(C)]`, little-endian, offsets only, no pointers):

| Offset | Field | Type | Writer |
|---|---|---|---|
| 0 | magic `"UWS1"` | [u8; 4] | helper, at creation |
| 4 | layout version (1) | u16 | helper, at creation |
| 6 | reserved | u16 | |
| 8 | size of the block in bytes | u32 | helper, at creation |
| 12 | reserved | u32 | |
| 16 | `seq` | u64 | helper |
| 24 | `applied_seq` | u64 | overlay |
| 32 | `edit_mode` (0/1) | u8 | helper |
| 33 | `visible` (0/1) | u8 | helper |

Fields are read and written under the state lock. If the helper's `CreateFileMappingW` returns
`ERROR_ALREADY_EXISTS` (a game-held block survived a helper restart), it checks magic, version and
size and refuses a mismatch with `version_mismatch` ("restart the game"); it never rewrites a
block it did not create. Plan 3 adds the hotkey table and widget list as layout v2.

**Overlay pipe messages.** The 16-byte `UWP1` header of plan 1 (magic, type, version, pid,
payload length; payload ≤ 4 KiB). Overlay types start at 0x0100, so a message sent to the wrong
pipe is rejected.

| Type | Name | Direction | Payload |
|---|---|---|---|
| 0x0101 | `overlay_hello` | game → helper | `module_version` u32, `state_layout` u16, reserved u16 |
| 0x0102 | `ping` | helper → game | `nonce` u32 |
| 0x0103 | `pong` | game → helper | `nonce` u32, reserved u32, `seen_seq` u64 |

The first message on the overlay pipe must be `overlay_hello`; a header version or
`state_layout` the helper doesn't support gives `version_mismatch`.

**Linux ↔ helper (JSON lines).** Every command carries a `request_id` (u32), echoed by its reply.

| Command | Reply |
|---|---|
| `load_overlay {path}` | `status {request_id, code, detail, message}` |
| `set_state {edit_mode?, visible?}` | `state_set {request_id, seq}`. If nothing changed: the current `seq`, no bump, no bell. |
| `ping` | `pong {request_id, seen_seq}`; or `error {request_id, code}`: `overlay_not_connected` (no overlay), `overlay_not_responding` (no `pong` within 15 s; a later `pong` is discarded) |

Unsolicited events: `bootstrap_connected {overlay_loaded, overlay_path}`,
`overlay_connected {module_version}`, `overlay_disconnected`, `bell {bits, applied_seq}` (unknown
bits named `bit<N>`), `bootstrap_disconnected`, `error {code}`, `game_exited`.
`uw-host` adds its own: `helper_restarted {attempt}`, `helper_protocol_error {line}`,
`overlay_timeout`, `stopped {reason}`.

Error codes (`error {request_id?, code, detail?}`; `request_id` when it answers a command):
- Replies: `overlay_not_connected`, `overlay_not_responding`, `bootstrap_not_connected`
  (`load_overlay` with no bootstrap), `state_busy`, `status_timeout`, `path_too_long`,
  `version_mismatch`.
- From the helper, unsolicited: `already_running`, `no_window`, `hook_failed` (`detail`: the
  `GetLastError`), `bootstrap_timeout`, `version_mismatch`, `pipe_error`, `channel_failed` (an
  object or the overlay pipe could not be created).
- From `uw-host`: `game_not_found`, `no_wine_session`, `helper_start_failed`; and a command still
  pending when its helper exits is answered `bootstrap_not_connected` or `overlay_not_connected`.
- `uw-host` stops (`stopped {reason}`, no restart) on `already_running`, `version_mismatch`,
  `hook_failed`, `bootstrap_timeout`, `game_exited` and a helper it cannot start; after any other
  helper exit it restarts the helper.

No queueing while disconnected: commands that need the overlay fail with
`overlay_not_connected`, and `uw-host` pushes the full desired state on the next
`overlay_connected`.

## Plan 1 items

- Late reply (final review M2): the bootstrap already echoes `request_id` in `status`. The helper
  discards a `status` whose id isn't the pending one and keeps waiting until its timeout.
- `bootstrap_connected` carries `overlay_path` (M3).
- A failed pipe write is reported as `bootstrap_disconnected`, not `game_exited`.
- A helper stdout line `uw-host` can't parse becomes `helper_protocol_error` (not dropped).
- Task 3 N1: the two Task 3 fixes get their own tests.

## Failures

| Case | Result |
|---|---|
| `status ok` but no `overlay_connected` within 10 s | `uw-host`: `overlay_timeout`. The overlay keeps retrying. |
| Overlay pipe drops | `overlay_disconnected`; the helper listens again; the overlay reconnects. |
| Helper exits while the game runs | `uw-host` restarts it after 1, 2, 4, 8 s, then every 10 s; the delay resets after 60 s of uptime. This includes a helper that exited after `no_window`, `channel_failed` or `pipe_error`. The marker makes the new helper skip the hook; both clients reconnect; `uw-host` reconciles. |
| `uw-host` exits or dies | The helper exits when its stdin closes, in every phase, so no orphan holds the helper mutex. |
| Game exits | `game_exited`, helper exit 0; `uw-host` emits `stopped` and doesn't restart. |
| `already_running`, `version_mismatch`, `hook_failed`, `bootstrap_timeout` | Reported once; `uw-host` emits `stopped`, no restart loop (for the hook errors, each restart would set a new hook on the game's UI thread). |
| `ping` not answered within 15 s | `error {request_id, code: "overlay_not_responding"}`; a `pong` that comes later is discarded, so the command still gets exactly one reply. |
| State lock not acquired within 100 ms (helper or overlay worker) | That command fails with `state_busy`. Game threads never wait. |
| Lock returns `WAIT_ABANDONED` | Treated as acquired (the other side died holding it); the header is re-checked before use. |
| Unknown type or bad message on the overlay pipe | `pipe_error`; the connection is dropped and the helper listens again. |

## Differences from Overwolf

From the owner's research (`~/Dev/underwolf-research`, docs 01 and 02).

Forced by Linux (owner ruling 2026-10-21):
- The channel lives in a Wine-side helper that relays to Linux (Linux can't open Wine's pipes or
  shared memory), one helper per game (each game has its own Wine session), so pipe names carry
  the pid and allow one instance; Overwolf's single server allows 255.
- The relay is a process that can die, so `uw-host` restarts it and the helper exits when its
  stdin closes.
- No window messages to the browser windows: the Electron windows are on Linux; plan 3 forwards
  input through the channel.

Choices (owner-accepted 2026-10-23):
- Framing: plan 1's header (magic, version, payload length; ≤ 4 KiB) instead of raw structs with
  no length (≤ 60 KiB), so a damaged or foreign message is rejected.
- Bootstrap commands are pipe messages, load only (plan 1); the overlay can't be unloaded.
- Doorbell masks use atomic OR/swap instead of a mutex, so game threads can ring without
  blocking.
- The state block is versioned in its header and starts small, instead of a ~124 KB blob versioned
  by name.
- `seq`, `applied_seq`, `state_applied`, request ids and ping/pong are added (not seen in
  Overwolf) for confirmation and liveness across the relay.
- Default pipe security plus local clients only, instead of a NULL DACL.
- Clients retry once a second without limit, instead of 5 × 5 s.
- No shared-memory mailbox transport.
- No queue while disconnected: a full state push on reconnect.

## Platform facts the design relies on

Checked 2026-10-23 against Microsoft Learn (Context7's Win32 index returned only stubs for these
pages, so the pages were read directly):

- `CreateEventW` with `bManualReset = FALSE`: "the system automatically resets the event state to
  nonsignaled after a single waiting thread has been released"; "If no threads are waiting, the
  event object's state remains signaled", so a ring is not lost if the receiver is busy. A name
  that exists returns the existing object, `ERROR_ALREADY_EXISTS`, and ignores the reset and
  initial-state arguments. "The event object is destroyed when its last handle has been closed."
- Events, mutexes and file mappings "share the same namespace": a name used by another kind fails
  with `ERROR_INVALID_HANDLE`. Hence the per-kind suffixes.
- `CreateFileMappingW(INVALID_HANDLE_VALUE, …)` needs an explicit size and is backed by the paging
  file; "The initial contents of the pages … are 0 (zero)". If the object exists, it returns "a
  handle to the existing object (with its current size, not the specified size)" and
  `ERROR_ALREADY_EXISTS`. The object lives until every view is unmapped and every handle closed,
  so a block held by the game survives a helper restart.
- `MapViewOfFile`: views in different processes are coherent; "Do not store pointers in the
  memory mapped file; store offsets".
- Interlocked access: "The threads of different processes can use these functions if the
  variable is in shared memory"; aligned 64-bit reads and writes are atomic on 64-bit Windows.
  The 64-bit masks sit at 8-byte-aligned offsets of a page-aligned view.
- `WaitForSingleObject`: `WAIT_ABANDONED` means the owner ended without releasing; ownership is
  granted and "you should check it for consistency".
- `CreateNamedPipeW`: `FILE_FLAG_FIRST_PIPE_INSTANCE` makes a second instance fail with
  `ERROR_ACCESS_DENIED`; `PIPE_REJECT_REMOTE_CLIENTS` rejects remote clients; the default
  security descriptor gives full control to LocalSystem, administrators and the creator and read
  access to Everyone, so other users can't open it for writing.
- Wine: named shared memory, events and mutexes shared between a Wine-side helper and the game
  are proven by Spike 3's channel. Lock-free atomics across processes on a Wine mapping are not
  yet proven; the Wine tests cover them.
- Bindings: `windows-sys` 0.61.2: `CreateEventW`, `CreateMutexW` in `Win32_System_Threading`;
  `CreateFileMappingW`, `OpenFileMappingW`, `MapViewOfFile` in `Win32_System_Memory`.

## Code layout

| Path | Contents |
|---|---|
| `crates/uw-pipe-proto/src/` | `pipe.rs` + overlay messages; `names.rs` + new names; new `bells.rs` (bits, names), `state.rs` (layout v1, header check on byte slices); `jsonl.rs` + new commands and events with `request_id` |
| `crates/uw-game-channel/` | new lib (Windows): worker, connect and reconnect, objects, callback API |
| `crates/uw-helper/src/` | `uw-helper64.rs` (663 lines) is split by job: bootstrap pipe, overlay pipe, objects, relay loop |
| `crates/uw-host/` | new lib (Linux): `discovery.rs`, `driver.rs` (moved from `uw-inject`), `supervisor.rs` (restarts, reconcile), public API |
| `crates/uw-inject/` | CLI only, on `uw-host` |
| `crates/uw-test-overlay/`, `crates/uw-test-game/` | test changes above |

## Testing

1. Unit tests (Linux): overlay message round trips and rejections (wrong first message, short,
   bad magic, types from the other pipe); state block offsets, size and header check; bit
   names; JSON commands and events; `uw-host` with a fake helper: request-id matching, restart
   delays and reset, reconcile after reconnect, stop rules, `helper_protocol_error`; the two
   Task 3 tests.
2. Wine end-to-end (`xvfb-run -a`, own prefix per test, `Drop` guards kill game, helper and
   wineserver; `--test-threads=1`): overlay connects; `set_state` → `state_applied` with
   `applied_seq == seq`; `ping` → `pong`; kill the helper → `uw-host` restarts it, the overlay
   reconnects and applies the reconciled state; a slow overlay start → `status_timeout`, then the
   late `status` is discarded and the next `load_overlay` gets its own reply; `--bad-state-block`
   → `version_mismatch`; closing the helper's stdin → it exits.
3. Owner live check, Deadlock under Proton: `uw-inject` connects the test overlay; edit mode on
   and off each give `state_applied`; `ping` → `pong`; the controller kills the helper and all is
   back within a few seconds; the game keeps running.

## Done when

All tests above pass, the owner's live check passes, no code under `crates/` calls
`CreateRemoteThread`, `WriteProcessMemory` or `VirtualAllocEx`, and clippy `-D warnings` is clean
for the host and the Windows target.

## Risks

- Lock-free atomics on a Wine shared mapping across processes: expected to work (Wine maps the
  same pages), not yet proven; the Wine tests prove it before the live check.
- Version pinning: the overlay, like the bootstrap, stays in the game for its session, so a
  client update that changes the overlay protocol or the state layout needs a game restart
  (`version_mismatch` says so).
- Splitting `uw-helper64.rs` touches plan 1's reviewed code; plan 1's Wine suite must stay green.
- Proton: only Proton 11.0 tested.

## Live check (owner)

2026-10-23, Deadlock ClientVersion 6819, Proton 11.0, Hideout, a fresh game session. **Pass.**

- Connect: `[uw-helper64] hooked WH_GETMESSAGE thread=308`, then `bootstrap_connected`
  (`overlay_loaded: false`), `status ok` "overlay started", `overlay_connected`
  (`module_version: 1`), `state_set` seq 1, and `bell` `state_applied` with `applied_seq` 1.
- Edit mode on, then off: `state_set` seq 2 and 3, each answered by `state_applied` with the same
  `applied_seq`. `ping` got `pong` with `seen_seq` 3. The test overlay's state file read
  `seq=3 edit_mode=0 visible=1`.
- Helper killed: back after 2 s. `helper_restarted` attempt 1, then `bootstrap_connected`
  (`overlay_loaded: true`, with `overlay_path`), `overlay_connected`, and the reconciled
  `state_set` with seq 3, which was unchanged, so no bell. `pong` with `seen_seq` 3. There was
  one `status` event in total (no second load), and the new helper skipped the hook.
- Stop: SIGTERM to `uw-inject`; the helper exited because its stdin closed.
- The game process (pid 1641867) kept running throughout. The owner saw no hitch.
