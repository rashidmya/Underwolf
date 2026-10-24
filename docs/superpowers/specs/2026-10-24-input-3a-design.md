# Input 3a — production keyboard and mouse capture

**Date:** 2026-10-24
**Status:** Approved by the owner on 2026-10-24; implementation plan next.

## Intent and decisions

Replace Spike 1's C3 input module with a production module using the injection and communication
work from plans 1, 2a and 2b. A player can enter exclusive input mode, use a test surface, and return
to the game without stuck keys, mouse movement leaking into the game, or a trapped cursor.

The owner accepted splitting roadmap item 3 into 3a and 3b. This document proposes the detailed
3a behavior. The owner approved this document after the builder check; the detailed choices
originated as the author's recommendations.
The parent design's Linux/Proton priority and read/draw-only constraints continue to apply.

## Approaches

1. **Build the whole input stack together.** One integration and live-check cycle, but keyboard,
   mouse, widget routing, hotkey configuration, DirectInput and GameInput failures overlap.
2. **Split the core input path from the broader routing/API surface — selected.** 3a establishes
   lifecycle, transport, keyboard/raw mouse and cursor restoration with an earlier live check;
   3b adds configurable hotkeys, widget hit-testing, the three passthrough modes, DirectInput and
   GameInput. Some protocol extension work will recur in 3b.

Keep the current named-pipe/helper architecture. Reusing the spike's TCP sender would bypass the
reconnection work already completed and retain a known host-restart failure.

## Scope

In 3a:

- A real `uw-overlay` DLL with the existing `uw_overlay_start` ABI, backed by a portable
  `uw-input` policy core and Windows adapters. Validate the start parameter size, version and pid.
- Thread keyboard/mouse hooks and the game's window procedure, with a documented single owner
  for each input kind so an event is neither forwarded nor toggled twice.
- MinHook detours for keyboard-state queries, raw-input reads and registration, and cursor
  position, clipping, visibility and shape. No copied IAT patcher.
- Fixed Ctrl+Tab edit-mode toggle, plus host `set_state` requests. Input belongs to the entire
  test surface while editing; outside editing, game input passes through.
- Keyboard transitions, text from the window's character messages, client-pixel mouse position,
  five mouse buttons and both wheel axes delivered to a host-side test receiver.
- Correct release on focus loss, hide, disconnect, delivery failure, or window destruction.
- An isolated test-game harness and an owner-observed Deadlock check.

3b owns configurable hotkey tables, widget targeting and per-widget passthrough behavior,
DirectInput and GameInput, and any low-level hook fallback found necessary beyond the thread
hooks. No claim of full input isolation for games using those deferred APIs is made in 3a.
The existing C3 evidence targets Deadlock's Win32/raw-input path under Proton 11.0.

Rendering optimization remains plan 4. The test surface may reuse the existing spike renderer
and Electron host behind a test adapter, but production crates must not depend on spike crates.
The adapter supplies cursor/edit-mode feedback and a typing/clicking surface; it installs no C3
DLL, runs no second input gate, and does not launch the old injector.
No launcher, app API runtime, provider port, Windows release, or 32-bit support is included.

## Current integration points

- `uw-game-channel/src/worker.rs` owns the overlay pipe, state reads and acknowledgements.
  It currently has only a state callback, no disconnect callback or outgoing input queue.
- `uw-pipe-proto` has overlay hello/ping/pong messages and a 64-byte state v1 block. Input
  events and actual capture state are absent.
- `uw-host/src/driver.rs` stores desired edit mode and reapplies it after reconnect, including
  commands issued while disconnected. That behavior must change for production input capture.
- The helper writes JSON lines synchronously. A slow reader can stall its control relay;
  capture must therefore have a game-local timeout independent of helper progress.
- `uw-test-overlay` remains the stand-in for communication tests; the new module has separate
  tests. Existing protocol, injection and data-channel guarantees remain regression gates.

## Ownership and capture lifecycle

The game UI thread owns capture transitions, the window procedure and cursor restoration.
Hooks that run on other threads use a bounded, nonblocking path to that owner and an atomic
capture flag. They never wait for IPC, a shared-memory mutex, or a host acknowledgement.
No Win32 calls run under the policy lock; recursive calls use original trampolines.

The policy distinguishes desired edit mode from **actual capture**. States are inactive,
entering, capturing, releasing and unavailable. Entry requires a foreground game window,
visible overlay, healthy input delivery, and successful installation of every required hook.
Capture is enabled only after registration/cursor setup and held-input release succeed.
Partial setup unwinds its own changes and reports failure.

Ctrl+Tab is evaluated from real input, before game-facing key-state filtering. It triggers once
per Tab press, works with either Ctrl, and is independent of key repeat. The chord is consumed.
Track physical state separately from what the game has seen. On entry release the keys/buttons
the game believes held; during capture game-facing queries cannot report those keys down.
On exit suppress keys/buttons still physically held until their release, then allow the next
fresh press. This prevents a held movement key or exit chord from restarting an action.

Any focus loss, hidden overlay, broken link, expired delivery lease, queue overflow affecting
discrete events, or window destruction immediately clears permission to capture and requests
local restoration. Regaining focus or reconnecting never re-enters automatically. Require a
fresh Ctrl+Tab press or a fresh accepted host edit-on request after the loss. Reject edit-on
while unavailable instead of saving it for later. Visibility and overlay-path reconciliation
continue as before. The host learns actual state and clears stale desired edit mode on loss.

An input epoch changes after every release/reconnect. Commands and events from an earlier epoch
cannot reactivate capture or reach the current receiver. The receiver clears its held state on
an epoch change. This prevents late queued events from continuing a drag after reconnect.

## Input and restoration contracts

The Win32 adapter must define a hook coverage table in the implementation plan: target API,
thread, original-call behavior, game-visible result during capture, and failure test.
Required coverage includes `GetKeyState`, `GetAsyncKeyState`, `GetKeyboardState`,
`GetRawInputData`, `GetRawInputBuffer`, `RegisterRawInputDevices`, `GetCursorPos`,
`SetCursorPos`, `ClipCursor`, `GetClipCursor`, `ShowCursor`, `SetCursor` and `GetCursor`.
The game sees neutral pressed state and a frozen cursor while the overlay uses real input.
Toggle-key bits and API return/error contracts remain valid; failed original calls are not
rewritten into success. Text follows character messages, including UTF-16 surrogate pairs;
full IME composition support is deferred to the app runtime.

Read raw input through the original function first. Preserve size queries, errors, record
counts and record boundaries. Neutralize keyboard/mouse payloads while capturing, preserving
required cleanup and unrelated HID records. Handle single and buffered reads, variable-size
records, and alignment; never treat a byte count as a record count. Establish a canonical
forwarding source per event type and test that simultaneous legacy and raw delivery produces
one host event. Preserve tagged synthetic releases without feeding them back as user input.

Capture changes only the keyboard/mouse registrations it needs. Save original flags and target
windows; remove `NOLEGACY` only where needed for the overlay path. Record successful game
registration changes made during capture and restore the latest game-requested configuration,
including removals. A failed registration call must not update that record. If there are no
intervening game requests, restoration is byte-for-byte equivalent to the original registration.
Do not register a new receiver window that steals the game's raw-input stream.

Maintain game-requested cursor position, clip, shape and display-count changes separately from
overlay changes. Restore the game view on normal exit and balance only Underwolf's display-count
adjustments. Never force a guessed `ShowCursor` count. On focus loss release confinement and do
not warp the pointer or reapply the game's clip over another application. Any deferred restoration
on focus return must use the current window and current game requests, not an obsolete rectangle.
Destroying the window discards its saved cursor state.

Create disabled MinHook hooks and publish trampolines before enabling. Batch owned hooks where
supported, but do not assume partial enable failure is atomic. Roll back only this module's
hooks; never disable all process hooks or uninitialize an engine another component uses.
No work happens under `DllMain`. A successful module remains loaded for the game session;
ordinary capture transitions change policy, not detour installation. Keep trampolines alive
while callbacks can execute. Hook panics cannot cross an FFI boundary; the recovery path releases
capture and reports the failure. Memory faults are not claimed to be catchable Rust panics.

## Transport and liveness

Extend the overlay control protocol with input batches, actual-state notifications and delivery
acknowledgements. Keep 2b's throttled game-data channel separate: keystrokes are not game events.
Use bounded queues, fixed-size records or bounded text payloads, per-epoch sequence numbers and
strict decode validation. The implementation plan assigns message tags, limits and byte layouts.
Negotiate input capability explicitly; an old helper/module pair cannot silently enable capture.
Do not reinterpret the existing v1 state bytes as a new layout.

The hook enqueues without blocking; the worker handles pipe I/O. Coalesce adjacent mouse moves
only within an uninterrupted run of moves. Preserve key/button/text/wheel ordering. If a
discrete event cannot be queued, release capture and invalidate the epoch instead of silently
dropping a release. Status/release signaling must have capacity independent of mouse traffic.

The Linux receiver acknowledges delivery progress, including an idle heartbeat. The helper
forwards that acknowledgement; successful pipe writes alone do not prove the receiver is alive.
Proposed defaults: acknowledgement/heartbeat every 100 ms; capture lease expires after 500 ms
without progress. A game-local timer checks the lease independently of the IPC worker, whose
existing write timeout can be five seconds. Clearing capture permission is immediate on observed
failure; restoration is posted to the UI thread. If that thread is stalled, no implementation can
promise UI restoration until it runs again; document and test this boundary explicitly.

State application acknowledgements must mean the UI thread applied the request, not merely
that the communication worker queued it. Actual-state events report epoch, active flag, reason
and the originating state sequence when applicable. Hotkey changes update actual state directly;
the host synchronizes desired state without echoing a stale command back into the game.

## Proposed code boundaries

| Area | Responsibility |
|---|---|
| `uw-input` | Portable policy, held-state accounting, transitions, queue/epoch rules; Windows hook adapters behind cfg |
| `uw-overlay` | Validated entry ABI, input installation and lifecycle, communication wiring |
| `uw-pipe-proto` | Versioned input records, capability and acknowledgement messages, JSON events/commands |
| `uw-game-channel` | Input queue servicing, connection callbacks and outgoing control messages |
| `uw-helper` | Bounded input relay and acknowledgement forwarding |
| `uw-host` / `uw-inject` | Actual-state tracking, no stale capture replay, receiver progress acknowledgements |
| Test game / test receiver | Exercise real hook behavior and visualize delivered input |

Exact module subdivision, queue implementation and protocol encoding are builder choices.
The plan must resolve their limits and concurrency ordering before implementation. Dependencies
should follow the production workspace's Windows bindings; the spike's MinHook wrapper is a
starting point to verify, not a reason to bring spike crates into production.

## Verification and completion

1. Portable tests cover entry rollback, both Ctrl keys, repeat suppression, held-input releases,
   no re-press before release, duplicate sources, focus/hide/loss transitions, epochs, stale
   commands, ordered delivery, overflow and lease expiration under a fake clock.
2. Wine tests use the real DLL in a dedicated test game. Exercise original API return contracts,
   raw size/error/record handling, key polling, registration changes during capture, cursor
   restoration, hook setup failure and repeated mode changes. Verify no hooks install in the
   helper merely because it loads the bootstrap.
3. End-to-end tests kill the helper and receiver while capturing, stall stdout without closing
   it, reconnect, and verify release without automatic recapture. A healthy idle receiver keeps
   the lease alive. Late acknowledgements/events from the old epoch are rejected. Run all 2a/2b
   regression suites; update tests only where this design explicitly changes capture replay.
4. The owner checks a fresh Deadlock session on Proton 11.0: instant toggle and visible cursor;
   no movement/look/fire during capture; typing/click/wheel delivery once; no stuck input on exit;
   normal focus/capture after alt-tab; helper kill and receiver stall return control without
   a game restart. Record build, desktop session, results, observed hitches and restore latency.
   Reuse the previous KDE/GNOME coverage matrix where those sessions are available; report any
   untested configuration explicitly.
5. Host and Windows target build/test/clippy checks pass. No game-data/code modifications or
   remote-thread injection are introduced. Compare enabled-but-idle input overhead and active
   capture behavior against the baseline; report measurements without claiming the full-overlay
   3% budget has been met (that remains plan 4's gate).

3a is complete only when the automated gates and owner-observed live check pass. A simulator
pass is not a substitute for evidence about Proton input, cursor behavior or perceived stutter.

## Sources and evidence

- Local evidence: `docs/spikes/01-linux-input.md`, `docs/spikes/SUMMARY.md`, the input spike,
  `uw-game-channel` worker, helper relay, host driver, and the recorded 2b live check.
- [MinHook upstream](https://github.com/tsudakageyu/minhook): queried through Context7 for
  disabled hook creation, trampoline publication and queued hook changes.
- Context7's Win32 queries returned incomplete or irrelevant details; checked the primary
  references directly on 2026-10-24:
  [raw registration](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerrawinputdevices),
  [buffered raw reads](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawinputbuffer),
  [cursor display count](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showcursor),
  [cursor confinement](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-clipcursor).
  These constrain API handling; they do not establish Wine/Proton behavior without testing.

## Builder check

2026-10-24: a separate read-only reviewer checked whether consequential unanswered questions
prevented an implementation plan. Result: "nothing important left". The owner subsequently
approved the proposed behavior.
