# Spike summary

Linux only (plan Amendment C). For the owner's review before any milestone plan is written.

## Verdicts

| Spike | Verdict (from the findings doc) | Mechanism chosen | Findings |
|---|---|---|---|
| 1: Linux input under Proton | **GO — H1′ + C3.** | In-game Win32 hook (C3): `WndProc` subclass for the keyboard and Ctrl+Tab; IAT hooks of `GetRawInputData`/`GetRawInputBuffer` in every loaded module for the mouse. Loaded by runtime injection (remote `LoadLibraryW` from inside the game's Wine session). Hook → host over localhost TCP. | [01-linux-input.md](01-linux-input.md) |
| 2: Shared-texture compositing | **Linux: GO, with owner-accepted conditions.** | Per-user Vulkan implicit layer, active only for `deadlock.exe`, drawing inside the game's present. Widgets: Electron CPU offscreen rendering → memfd slots → host-pointer import → GPU copy → textured quads (CPU copy, Amendment A). | [02-shared-texture.md](02-shared-texture.md) |
| 3: Deadlock live data | **Linux: GO, with owner-accepted deviations.** | Collector plugin of the Spike 1 in-game module. One MinHook detour on `ntdll!RtlQueryPerformanceCounter` (QPC); passes run on the game's main-loop thread. Roots from masked byte patterns, fields by schema name. Overwolf-style named shared memory, relayed by a Wine-side helper to `uw-gep`. | [03-deadlock-memory.md](03-deadlock-memory.md) |

## Decisions needed from owner

From the findings docs:
- **01:** none open (DLL load method, transport and C3's input surface are decided there).
- **02:** whether the ~3% budget applies to the layer alone or to the whole overlay including
  Chromium, and which optimisations come first.
- **03, 1. Hideout/sandbox match id:** the game gives none there, so no `match_start`/`match_end`.
  Keep emitting nothing, or have the provider generate a local id (as Overwolf's handler does)?
- **03, 2. Unavailable features:** accept `incoming_damage` and `match_history` as unavailable, or
  choose another source?
- **03, 3. `tower_destroyed` accuracy:** accept it on the owner-observed subset, or verify it on a
  matchmade match first?

Plan's minimum list:
- **AMD verification:** not done. Spikes 1 and 2 ran on NVIDIA only (no AMD hardware). 02: AMD is
  required before the milestone 1 release, including whether Electron's GPU shared textures work
  there (they would remove the CPU copy).
- **Proton-version fragility of injection and input:** only Proton 11.0 was tested (01, 03), and
  not Proton's Wayland driver (01). 01 rates the parts:
  - Joining the game's Wine session from outside (wineserver socket layout, the container sharing
    `/tmp` and the PID namespace, matching `WINE*SYNC`): medium risk. The launcher refuses to run
    if it can't find the session.
  - The injection itself (standard Win32): low risk.
  - The hook (`WndProc` subclassing, `WM_KEY*` delivery, SDL3's statically imported
    `GetRawInputBuffer`): low risk from Proton, moderate from game updates.
- **Spike 3 features unavailable or unverified (03):**
  - Unavailable: `incoming_damage`, `match_history`.
  - Unverified: the full `tower_destroyed` count (22 events; the deadlock-api metadata never came).
  - Not yet through a patch: the rows added on 6810 (`game_mode`, `team_score`, `match_outcome`,
    the `game_paused` true/false event, `tower_destroyed`) and the `tier0`/`engine2` main-loop
    patterns.

## Not scheduled

Windows: Vulkan layer on Windows, DX11 hook, Windows installer, code-signing. Tasks 14–16 were
dropped (Amendment C), so Spike 2 (Windows Vulkan, DX11) and Spike 3 (Windows) have no result.

## Proposed milestone 1 plans, in order

1. **Injection** (spec §6.3 "Getting in"). Helper per bitness, bootstrap DLL with a
   thread-targeted `SetWindowsHookEx(WH_GETMESSAGE)` (`WH_CALLWNDPROC` fallback) plus wake-up
   messages, self-pinning, load-overlay on command. Replaces the spike's remote `LoadLibraryW`.
   - Proved (01): an outside injector, run with the game's Proton inside its Wine session, loaded
     the module into the running Deadlock with no game-side setup (Proton 11.0, SLR 4). The
     pressure-vessel container shares the host's PID namespace and `/tmp`.
   - Proved (03): the collector loads as a plugin of that module (`LoadPlugin`).
   - Known gap (01): after a host restart the hook stops forwarding and re-injection is a no-op;
     the module must reconnect. (03): the plugin can't be unloaded; a failed start needs a game
     restart.
2. **Communication** (spec §6.3 "Client ↔ in-game channel"). Duplex named pipe of fixed-layout
   messages (hello first), named auto-reset events plus a shared bitmask as doorbells, and the
   shared state block, all owned by the Wine-side helper, which relays them to the Linux host.
   Replaces the spike's localhost TCP to the module; the Spike 3 data-channel relay folds into the
   same helper.
   - Proved (01): hook → host over localhost TCP works under Proton.
   - Proved (03): a Wine-side helper started by the Linux client with the game's Proton and prefix
     opens Wine's named shared memory and relays it over TCP. After helper or client restarts the
     full snapshot is back within ~1 s and the game keeps running.
   - Known issue (03): a reattaching helper replays the ring's last ~30 events; a production
     consumer must start at the write head or mark replays.
3. **Input layers** (spec §6.3 "Input, all of Overwolf's layers"). Every hook on MinHook (moving
   C3's IAT hooks), thread keyboard/mouse hooks, key-state, raw-input read/registration with exact
   restore, cursor position/clip/visibility/shape, DirectInput, GameInput, the client's hotkey
   table, in-module hit-testing, and `passThrough`/`noPassThrough`/`passThroughAndNotify`.
   - Proved (01): C3 passes P1–P6 on KDE Wayland and GNOME Wayland (Proton 11.0, NVIDIA): the
     game sees nothing in edit mode, clicks and typing reach widgets, drag works, no stuck keys on
     exit, the game keeps focus and mouse capture. Deadlock reads the mouse through `SDL3.dll`'s
     `GetRawInputBuffer`.
   - Proved (03): a MinHook detour works in the game under Proton (the QPC hook).
   - Risks (01): keyboard assumed legacy `WM_KEY*`; raw-input functions assumed statically
     imported.
4. **Dirty-rect frames** (spec §6.3 "Widget frames"). Per-widget control block (size, frame
   counter, last-consumed frame, dirty rect, new/consumed flag) in Linux shared memory next to
   today's slots; the producer grows the dirty rect until the consumer catches up and the layer
   copies only dirty rows. Target: bring Spike 2's full-cost result under the 3% gate.
   - Proved (02): the CPU-copy path composites three widgets in Deadlock and survives host and
     game restarts, alt-tab, windowed ↔ fullscreen and resolution changes.
   - Cost (02): full overlay +2.74% FPS / +3.57% p99 (normal settings) and +3.07% / +2.50% (max
     settings), each failing one measure; layer alone +1.29% / +1.22% (PASS). About half is the
     layer, half Chromium plus the copies. No GPU-bound measurement was possible on this machine.
   - Lesson (02): the layer must never do device-wide waits.
5. **Then the Deadlock provider** (spec §6.4, from Spike 3), **the app runtime** (§6.2), **the
   launcher** (§6.6) **and the catalog** (§6.5).
   - Proved (03): the in-game collector reads `phase`, `match_id`, `steam_id`, `roster`, `items`
     (local count, bot match only), `match_clock`, `match_start`/`match_end`,
     `kill`/`death`/`assist`, `game_mode`, the `game_paused` event, `team_score` and
     `match_outcome` live. Cost +0.38% avg FPS, +0.14% p99 (PASS). Two `client.dll` patches
     (6801 → 6807 → 6810) needed no code change. Under deliberate corruption the first run
     published a wrong value; with the layout check added, the rerun switched the roster off
     instead (PASS).
   - Provider requirement (03): turn the collector's raw event data into Overwolf's documented
     shapes (`match_clock` as "01:00", null for kill/death/assist/match_start/match_end, the plain
     team name for `tower_destroyed`).
   - App runtime, launcher, catalog: not covered by the spikes.
