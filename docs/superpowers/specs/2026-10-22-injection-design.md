# Milestone 1, plan 1: Injection (design)

Date: 2026-10-22. Status: for owner review.
Implements spec §6.3 "Getting in" (`2026-10-20-underwolf-design.md`). Proposed in
`docs/spikes/SUMMARY.md`, milestone 1 item 1.

Owner decisions (2026-10-22): 64-bit only in this plan; approach A (injection plus a thin first
slice of the pipe; the full channel is plan 2).

## Goal

Load Underwolf's in-game code into a running game under Proton without remote-thread injection,
the way spec §6.3 describes: a helper process sets a thread-targeted message hook that makes the
game load a small bootstrap DLL; the bootstrap pins itself, connects to the helper, and on command
loads the overlay DLL and calls its start function. It replaces the spike's `CreateRemoteThread` +
`LoadLibraryW` and closes Spike 1's known gap (re-injection after a host restart was a no-op).

## Scope

In:
- Linux side `uw-inject`: find the game and its Wine session, start the helper inside it, drive it.
- Wine-side helper `uw-helper64.exe`: find the game's UI thread, hook, serve the pipe.
- Bootstrap `uw_bootstrap64.dll`: pin, connect, load and start the overlay, reconnect.
- The pipe's first messages (`hello`, `load_overlay`, `status`) in the format plan 2 extends.
- A test overlay DLL and a test game exe for automated Wine tests.

Out:
- 32-bit games (owner decision). Names and structure leave room for a `32` build later.
- The rest of the channel (doorbell events, shared state block, Linux relay): plan 2.
- The real overlay module and its `uw_overlay_start` (input hooks, collector plugin): plan 3.
- When to load the overlay (per-game setting, app needs): the client's policy, a later plan.
- Unloading the bootstrap or the overlay: not supported (Spike 3 showed a plugin can't be
  unloaded safely; the bootstrap is pinned on purpose).
- Native Windows: not scheduled (Amendment C). Helper and bootstrap are plain Win32, untested
  there.

## Components and flow

```
Linux: uw-inject ──spawns with the game's wine──▶ uw-helper64.exe (Wine, in the game's session)
         ▲  stdin/stdout JSON lines                    │ SetWindowsHookExW + wake-up messages
         └─────────────────────────────────────────────┤ named pipe \\.\pipe\underwolf-<pid>
                                                       ▼
                                  game process: uw_bootstrap64.dll ──LoadLibraryW──▶ overlay DLL
```

1. **Find the game (Linux, `uw-inject`).** Find the game process in `/proc` by its exe name and
   read its Wine session from its environment: `WINEPREFIX`, the Proton `files` directory (from
   `compatdata/<appid>/config_info`) and every `WINE*SYNC` variable. This is the spike launcher's
   logic, ported. Start the helper with the game's own Proton `wine` and that environment, which
   joins the game's running wineserver.
2. **Find the UI thread (helper).** Find the game inside Wine by exe name (Toolhelp snapshot),
   then its main window: top-level, visible, no owner, largest area. Its thread
   (`GetWindowThreadProcessId`) is the UI thread. No such window yet: check again every 500 ms.
3. **Already there? (helper).** If the named mutex `Local\underwolf-bootstrap-<pid>` exists, a
   bootstrap from an earlier helper is already pinned in the game: create the pipe server, skip
   the hook, and wait for its `hello` (step 7). Here and in every name below, `<pid>` is the
   game's Windows (Wine) process id, not its Linux pid.
4. **Hook (helper).** Take `Local\underwolf-helper-<pid>` (a second helper for the same game exits
   with `already_running`). Create the pipe server first, then `LoadLibraryW` the bootstrap into
   the helper itself (to get its module handle and hook procedure address), call
   `SetWindowsHookExW(WH_GETMESSAGE, proc, module, ui_thread)`, then wake the UI thread so its
   message pump runs the hook: post a few `WM_NULL` messages to it (`PostThreadMessageW`, picked
   up by its next `GetMessage`/`PeekMessage`) and send one `WM_NULL` to its main window
   (`SendNotifyMessageW`), because the fallback `WH_CALLWNDPROC` fires only on sent messages. Both
   are standard no-op messages, delivered by the game's own pump; the helper repeats them every
   500 ms while it waits. Wine then loads the bootstrap into the game and calls the procedure there.
5. **Bootstrap start (game process, game's UI thread).** On the first call only (atomic flag):
   pin the module (`GetModuleHandleExW(FROM_ADDRESS | PIN)`), start one worker thread, then
   `CallNextHookEx` and return. Every later call is just `CallNextHookEx`. Nothing else runs on the
   game's thread.
6. **Connect (bootstrap worker).** Create the marker mutex, connect to the pipe (retry once a
   second while the pipe is missing or busy), send `hello`.
7. **Unhook (helper).** On `hello`, `UnhookWindowsHookEx` (the pin keeps the bootstrap loaded) and
   report `bootstrap_connected` on stdout.
8. **Load the overlay.** `uw-inject` sends `load_overlay <linux path>`; it converts the path to the
   Wine `Z:\` form. The helper forwards it; the bootstrap worker calls `LoadLibraryW`, looks up
   `uw_overlay_start`, calls it, and replies `status`. The same path again answers
   `already_loaded` without reloading.

## Messages

**Pipe (helper ↔ bootstrap).** Fixed-layout binary, little-endian (spec §6.3). Plan 2 adds
message types to this format; it does not replace it. The helper creates the pipe with
`PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE`, message mode, `PIPE_REJECT_REMOTE_CLIENTS`,
one instance.

- Header, 16 bytes: `magic` = `"UWP1"`, `type: u16`, `version: u16`, `pid: u32`,
  `payload_len: u32`.
- Payload at most 4 KiB. A bad magic or an oversized payload closes the connection (the bootstrap
  reconnects). An unknown type gets `status` `unsupported`.
- Strings: paths are UTF-16 with a `u16` length prefix, at most 1024 code units; status messages
  are UTF-8 with a `u16` length prefix, at most 256 bytes.

| Type | Direction | Payload |
|---|---|---|
| `hello` (first message) | bootstrap → helper | protocol version, bootstrap version, bitness (64), `overlay_loaded` (bool), overlay path |
| `load_overlay` | helper → bootstrap | request id, Windows path |
| `status` | bootstrap → helper | request id, code, detail (`u32`: Windows error or the start function's code), message |

Status codes: `ok`, `already_loaded`, `already_loaded_other`, `load_failed`, `no_start_export`,
`start_failed`, `unsupported`.

**Overlay entry point.** `extern "system" fn uw_overlay_start(params: *const StartParams) -> u32`,
0 on success, otherwise the overlay's own error code. `StartParams` starts with `size: u32` and
`version: u32` so fields can be added; in this plan the only other field is the game's pid. It is
called on the bootstrap's worker thread, never the game's thread; the overlay starts its own
threads and hooks from there. The overlay's `DllMain` must do nothing (loader lock).

**Linux ↔ helper (temporary; plan 2's relay replaces it).** One JSON object per line. Commands on
the helper's stdin, e.g. `{"cmd":"load_overlay","path":"Z:\\…\\overlay.dll"}`. Events on its
stdout, e.g. `{"event":"bootstrap_connected","overlay_loaded":false}`,
`{"event":"status","code":"ok"}`, `{"event":"error","code":"no_window"}`. Diagnostics go to stderr.

## Failures and reconnecting

Rule: a failure is reported, never retried blindly, and never crashes the game. No Rust panic
crosses an FFI boundary: the hook procedure, the worker thread and `uw_overlay_start`'s caller
catch panics and turn them into a reported error. The bootstrap is built with `panic = "unwind"`
so `catch_unwind` works.

| Situation | What happens | Reported as |
|---|---|---|
| Game or its Wine session not found | No helper started | `game_not_found` / `no_wine_session` (Linux) |
| No game window within 120 s | Helper exits | `no_window` |
| `WH_GETMESSAGE` can't be set | Try `WH_CALLWNDPROC` | `hook_failed` (with `GetLastError`) if both fail |
| Hook set, no `hello` within 10 s despite wake-ups | Unhook, try the fallback hook once | `bootstrap_timeout` |
| `hello` with another protocol version | Refused; the pinned bootstrap stays until the game restarts | `version_mismatch` (tells the user to restart the game) |
| Overlay missing, no `uw_overlay_start`, or start fails | Bootstrap stays connected; no automatic retry | `status` with code, detail and message |
| A different overlay path after one is loaded | Refused (no unload) | `already_loaded_other` |
| Second helper for the same game | Exits | `already_running` |
| Game exits | Pipe breaks, helper exits | `game_exited` |

**Reconnect after a client restart.** Ending the client ends the helper. The bootstrap's pipe
read fails and it returns to its connect loop. The new helper finds the marker mutex, skips the
hook, and receives a `hello` with `overlay_loaded: true` and the path, so the client does not
load the overlay again.

## Platform facts the design relies on

Checked 2026-10-22 against Microsoft Learn (via Context7 and the `SetWindowsHookExW` page):

- `SetWindowsHookExW`: when `dwThreadId` is a thread of another process, `lpfn` "must point to a
  hook procedure in a DLL" and `hmod` is that DLL's handle. So the helper loads the bootstrap into
  itself first, and the bootstrap's `DllMain` must do nothing: it also runs in the helper.
- "A 32-bit DLL cannot be injected into a 64-bit process … The 32-bit and 64-bit DLLs must have
  different names." Hence `uw_bootstrap64.dll`.
- `WH_GETMESSAGE` and `WH_CALLWNDPROC` can both be thread-scoped. Call `CallNextHookEx` every time.
- `GetModuleHandleExW` with `GET_MODULE_HANDLE_EX_FLAG_PIN`: "The module stays loaded until the
  process is terminated, no matter how many times FreeLibrary is called." Unhooking may otherwise
  unload the bootstrap. `PIN` can't be combined with `UNCHANGED_REFCOUNT`.
- Loader lock: `LoadLibrary`/`FreeLibrary` are not safe from `DllMain`. All loading happens on
  the bootstrap's worker thread.
- Pipe client: `CreateFileW` fails with `ERROR_FILE_NOT_FOUND` when the server pipe doesn't exist
  yet and `ERROR_PIPE_BUSY` when its instance is taken; both mean "retry later".
- Wine: whether a thread hook from another process loads the hook DLL into the target is not
  covered by Wine's documentation in Context7. Wine's `user32` hook code does load the module in
  the target when the hook is called, but this project has not proved it. Plan task 1 proves it.
- Bindings: the spikes use `windows` 0.58. The new crates use the latest released `windows-sys`
  (raw FFI, no COM needed); the plan checks its version and feature names at writing time.

## Code layout

New crates in `crates/`, added to the workspace. `spikes/` is not changed and nothing depends
on it.

| Crate | Builds | Purpose |
|---|---|---|
| `uw-pipe-proto` | library (Linux and Windows) | Message encode/decode, limits. Plan 2 extends it. |
| `uw-inject` | Linux library + `uw-inject` CLI | Game and session discovery, helper start, path conversion, JSON lines. |
| `uw-helper` | `uw-helper64.exe` (`x86_64-pc-windows-gnu`) | Window/thread lookup, hook + fallback, pipe server, mutexes. |
| `uw-bootstrap` | `uw_bootstrap64.dll` | Hook procedure, pin, worker, marker mutex, overlay load. |
| `uw-test-overlay` | test DLL | `uw_overlay_start` writes `%TEMP%\uw-test-overlay-<pid>.txt` (the test reads it through the prefix). |
| `uw-test-game` | test exe | One window and a standard message loop; a `--no-window` mode. |

The helper's decisions (wait, hook, fallback, skip when the marker exists, timeouts) live in
portable code behind a small trait over the Win32 calls, so they are unit-tested on Linux with a
fake, as the collector's tasks are with `FakeMem`.

## Testing

1. Unit tests (`cargo test`, Linux): protocol round trips and every rejection (bad magic,
   oversize, bad string length, unknown type); `/proc` and `config_info` parsing; path
   conversion; JSON lines; the helper's decision logic against the fake.
2. Wine end-to-end tests (ignored by default; need `wine`; plain Wine, not Proton), with
   `uw-test-game.exe`:
   - hook → `hello` → `load_overlay` → `ok` and the marker file;
   - same path again → `already_loaded`; another path → `already_loaded_other`;
   - kill the helper, start a new one → no new hook, `hello` with `overlay_loaded: true`;
   - forced `WH_CALLWNDPROC` (a helper flag) → same result;
   - `--no-window` game → `no_window`; overlay without the export → `no_start_export`.
3. Owner live check, Deadlock under Proton: `uw-inject` → `bootstrap_connected` → load the test
   overlay → marker; restart `uw-inject` → `hello` with `overlay_loaded: true`; the game keeps
   running normally throughout.

Plan task 1 is the proof of the hook mechanism under plain Wine with the test game, then in
Deadlock. If neither `WH_GETMESSAGE` nor `WH_CALLWNDPROC` loads the bootstrap, stop and report to
the owner (remote-thread injection is ruled out by the spec).

## Done when

All tests above pass, the owner's live check passes, and no code under `crates/` calls
`CreateRemoteThread`, `WriteProcessMemory` or `VirtualAllocEx`.

## Risks

- The Wine hook mechanism (above): proved first, or the plan stops.
- UI thread choice: the "largest visible top-level window without owner" rule fits Deadlock
  (one SDL3 window); a game with a launcher window first is handled by waiting until the rule
  picks the game window, which the live check confirms only for Deadlock.
- Version pinning: a bootstrap stays in the game for its whole session, so a client update that
  changes the protocol needs a game restart (`version_mismatch` says so).
- Proton: only Proton 11.0 tested (as in the spikes).
- Anti-cheat: VAC risk accepted by the owner (spec); a message hook is a standard Windows
  mechanism and no remote thread is created.

## Live check (owner)

2026-10-23, Deadlock ClientVersion 6819, Proton 11.0, Hideout, no launch options. **Pass.**

- First run: `[uw-helper64] hooked WH_GETMESSAGE thread=312`, then `bootstrap_connected`
  (`overlay_loaded: false`) and `status ok` "overlay started"; one marker
  `uw-test-overlay-308.txt` in the prefix's Temp. Ctrl-C ended `uw-inject` and the helper.
- Second run: `[uw-helper64] marker present; skipping hook`, then `bootstrap_connected`
  (`overlay_loaded: true`); still one marker.
- The game process (pid 21848) kept running through both runs. Smoothness was not watched.
