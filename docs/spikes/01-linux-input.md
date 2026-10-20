# Spike 1: Linux input under Proton

## Question

> Prove the overlay can capture mouse/keyboard in edit mode and pass it back to Deadlock
> afterwards, on KDE and GNOME Wayland sessions (the game itself runs through XWayland).
> (Session matrix per plan Amendment B.)

## Verdict

**GO — H1′ + C3.** The in-game Win32 input hook passes P1–P6 on both cells (KDE Wayland and GNOME
Wayland, Proton 11.0, NVIDIA). It is loaded the Overwolf way: the overlay host injects it into the
running game, with no game-side setup. C1 fails P6; C2 leaks the mouse.

## Evidence

Pass criteria (Task 7):
- P1: enter within 100 ms.
- P2: in edit mode the game sees nothing.
- P3: clicks (10/10) and typing reach widgets.
- P4: drag works.
- P5: clean exit, 20 cycles, no stuck keys.
- P6: the game doesn't minimise, pause or lose focus or mouse capture.

### H1 (XInput2 raw Ctrl+Tab, read-only, in the layer) + C1 (transparent always-on-top capture window from the host)

| Cell | P1 | P2 | P3 | P4 | P5 | P6 | Notes |
|---|---|---|---|---|---|---|---|
| KDE Wayland | ✅ instant | ✅* | ✅ | ✅ | ✅ (62 cycles) | ❌ | *The game ignores input only because it lost focus. The overlay window becomes the active window and Deadlock a background window (owner: "the overlay is not attached to the game"). |
| GNOME Wayland | not run | | | | | | C1 already fails P6 |

- H1 works: Ctrl+Tab is seen through XInput2 raw events from inside the game process while the
  game (an XWayland client) has focus. It also works while the XWayland capture window has focus.
- C1 verdict: **fails P6**. Dropped in favour of C2.

### H1 + C2 (filter input inside the game process at winex11.so's `XCheckIfEvent` / `XCheckTypedWindowEvent`; overlay input from the layer's XI2 raw stream; cursor drawn by the layer)

| Cell | P1 | P2 | P3 | P4 | P5 | P6 | Notes |
|---|---|---|---|---|---|---|---|
| KDE Wayland | ✅ instant, no window | ❌ keyboard blocked, **mouse leaks** (camera turns, clicks shoot) | ✅ | ✅ | ✅ (20 cycles) | ✅ game stays focused | Filter installed (2 imports redirected) |

- Why the mouse leaks: while a game has the mouse captured, Wine reads raw mouse motion and
  buttons in its separate desktop process (`explorer.exe`, which has its own `winex11.so`) and
  forwards them to the game. The keyboard goes through the game's own X connection, so the
  in-process filter catches it; the mouse never passes through the game process's X event reads.
- C2 verdict so far: **partial**. In-game, "attached to the game" (P6) works. Blocking the mouse
  needs the filter in Wine's desktop process too, or a filter at another point in the game
  process (see Decisions needed).

### H1′ + C3 (in-game Win32 input hook, injected DLL under Proton)

Owner decision after C2: use **C3**, a Win32-side input hook inside the Deadlock process (the same
mechanism planned for Windows, spec §6.3). It owns the hotkey itself, so it needs no separate H1.
Ported from the deleted `spike-c3-input` branch and finished here, with the raw-input hook the C3
review (I5) said P2 requires.

**What it is.** Deadlock is a Windows build; under Proton its input arrives through Wine as Win32
messages and raw input. The injected DLL:
- Subclasses the game window's `WndProc` for **keyboard**: Ctrl+Tab toggles edit mode (rising-edge,
  so auto-repeat doesn't re-toggle); in edit mode keys are swallowed from the game and forwarded to
  the host; on entry every held key is released to the game so nothing sticks (Review Focus 3 / P5).
- IAT-hooks `user32!GetRawInputData` for the **mouse**: Source 2 reads camera-look as raw input,
  which a `WndProc` subclass never sees — this is exactly why C2 leaked the mouse. The hook lets the
  real call fill the game's buffer, forwards the motion/clicks to the host, and while editing zeroes
  the buffer in place so the camera does not move (P2). Held buttons are released on entry via a
  tagged `SendInput` that the hook passes straight through.
- It touches only the input path — no game memory is read or written and no game logic is hooked
  (spec rule for C2/C3).

**How it is wired into the spikes pipeline (host-mediated TCP relay).** The Vulkan layer is a
native-Linux `.so` (winevulkan host side) and the hook is a Windows PE under Wine, so they can only
meet over a socket. The hook forwards `Hello`/`EditMode`/`Input` to the **host** over **localhost
TCP** (`127.0.0.1:48610`, `UW_SPIKE_HOST_PORT`); the host feeds them into the same edit/input
queues it already drains for C2, does the hit-testing / drag / widget routing, and relays edit-mode
and the cursor back to the **layer** over the existing Unix socket. In `--capture win32` the layer
installs no XI2 thread and no Wine filter; it only draws the dim + cursor, taking the cursor
position from the host (mouse motion is relative, so the host accumulates an absolute
swapchain-clamped cursor). Edit mode is **DLL-authoritative**: the host never self-exits on
Escape/click-outside, only the hook does.

**What is proven here.** The decision logic is portable and unit-tested on the Linux host: the gate
(`input/tests/gate.rs`, incl. P5 `exit_leaves_no_keys_held_in_game`), Win32 message translation and
orchestration, the raw-input parse/neutralize core (`input/tests/rawinput.rs`, incl.
`raw_mouse_is_blocked_from_the_game_and_forwarded_in_edit_mode` for P2), the TCP relay
(`osr-bridge/tests/bridge.rs` `c3_tcp_*`), and the transport framing (`ipc/tests/buffer.rs`). The
`cfg(windows)` FFI (WndProc subclass, `GetRawInputData`/`RegisterRawInputDevices` IAT hooks, tagged
`SendInput`, `DllMain`, Winsock transport) type-checks for `x86_64-pc-windows-gnu` but is **not**
run in CI — the owner verifies it on hardware.

**Pass/fail matrix (to be filled on hardware).** KDE Wayland and GNOME Wayland (plan Amendment B;
the game runs through XWayland in both).

| Cell | P1 | P2 | P3 | P4 | P5 | P6 | Notes |
|---|---|---|---|---|---|---|---|
| KDE Wayland | PASS | PASS | PASS | PASS | PASS | PASS | Run 2, 2026-10-21, Proton 11.0; hook injected by the host, no game-side setup |
| GNOME Wayland | PASS | PASS | PASS | PASS | PASS | PASS | Run 3, 2026-10-21, GNOME 51.1, Proton 11.0; 26 enter cycles; hook injected by the host |

**Build and load (for the hardware run).**
1. Install the mingw linker: `sudo pacman -S mingw-w64-gcc` (the `x86_64-pc-windows-gnu` Rust target
   is installed, but linking the DLL needs mingw; without it the DLL only type-checks).
2. `cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-input` →
   `target/x86_64-pc-windows-gnu/release/uw_spike_input.dll`.
3. Install the layer, hook, injector and launcher: `spikes/inject/install.sh` (reinstalls the
   Vulkan layer, builds the DLL and
   `uw-spike-inject-win.exe` for Windows and the `uw-spike-inject` launcher, and copies them to
   `~/.local/share/underwolf-spike/`). Loading needs no game-side setup: the host injects the DLL
   itself (see "How the DLL is loaded").
4. Start the host in C3 mode: `cd spikes/osr-host && npm start -- --capture win32 --widget input --widget static`.
   When the game's layer connects, the host runs `uw-spike-inject` and logs
   `C3: injector exited with 0 (hook loaded)`. To retry by hand: `~/.local/share/underwolf-spike/uw-spike-inject`.
5. Launch Deadlock with `UW_SPIKE_MODE=composite UW_SPIKE_CAPTURE=win32 %command%`, enter the
   Hideout, and fill in the matrix.

**How the DLL is loaded (owner decision: exactly what Overwolf does, no setup).** An outside
injector process makes the running game load the DLL: it writes the DLL's path into the game and
starts a remote thread on `LoadLibraryW` (`spikes/inject`, `uw-spike-inject-win.exe`; the same
binary is the Windows injector for milestone 2). Nothing goes into the game folder, the prefix or
the Steam launch options. The only Proton-specific part: the injector has to run inside the game's
Wine session to see `deadlock.exe`, so the Linux launcher (`uw-spike-inject`):
- finds `deadlock.exe` by argv0 and copies `WINEPREFIX` and the `WINE*SYNC` settings from its
  `/proc/<pid>/environ` (the sync mode must match the running wineserver);
- takes the game's Proton from `compatdata/<appid>/config_info` and runs the injector with that
  Proton's `files/bin/wine`;
- first checks that the game's wineserver socket exists (`/tmp/.wine-<uid>/server-<dev>-<ino>/socket`
  for the prefix); otherwise it stops, because Wine would start an empty session for the injector.
  Evidence that this works from outside Steam's container: Deadlock's prefix (dev `0x24`, inode
  `0x17e2e`) has its server directory `server-24-17e2e` in the host's `/tmp`, so the container
  shares `/tmp` with the host.
- Proven without the game: `spikes/inject/tests/wine_e2e.rs` (ignored by default) starts a Windows
  process under Proton 11's own `wine` in a scratch prefix, runs the launcher from outside, and the
  real C3 DLL loads and greets over TCP — PASS.
- **In Deadlock — PASS (2026-10-21, Proton 11.0, SLR 4):** with the game started normally (no
  launch options), `uw-spike-inject` from a plain terminal loaded `uw_spike_input.dll` into
  `deadlock.exe` (Wine pid 300, Linux pid 2242918; the DLL appears in `/proc/<pid>/maps`), and the
  game kept running. The pressure-vessel container has its own mount namespace but shares the
  host's PID namespace and `/tmp`, which is why the outside injector can join the session.
- The DLL cannot be unloaded; quitting the game removes it. If the host restarts while the game
  runs, re-injection is a no-op and the already-loaded hook stops forwarding (its sender exits when
  the host goes away) — restart the game too.

**Alternative found while researching (not built): Steam's own method.** Proton's `winex11.so`
drops all keyboard and mouse events (raw XI2 included) in every Wine process — `explorer.exe` too,
where C2 leaked the mouse — while the session-wide Wine event
`\Sessions\<id>\BaseNamedObjects\__wine_steamclient_GameOverlayActivated` is signalled
(`process_events` in `dlls/winex11.drv/event.c`, ValveSoftware/wine `proton_10.0`; the same strings
are in Proton 11's `winex11.so`/`lsteamclient.dll`: "HACK: Steam overlay activated, disabling all
input events"). Steam's overlay sets it; the Vulkan layer could set it with no DLL. Risks: key
releases dropped too (stuck keys), Steam's overlay closing resets it, absent from `winewayland`.

**In-game run 1 (2026-10-21, KDE Wayland) — invalid, two causes found:**
- The installed Vulkan layer predated the C3 wiring, so it ignored `UW_SPIKE_CAPTURE=win32` and ran
  C2 (Proton log: `input filter installed in winex11.so`) alongside the hook. Both toggled edit mode
  on Ctrl+Tab. Symptoms: W blocked (C2 filter), mouse leaked, no overlay cursor/clicks/typing, and
  after W-in-edit-mode + exit the game took no input until another enter/exit (the two edit modes
  out of sync — to re-check). Fix: `install.sh` now reinstalls the layer.
- The raw-input hook patched only `deadlock.exe`'s imports, which has no raw-input imports.
  Deadlock reads the mouse through `SDL3.dll`'s raw-input thread with `GetRawInputBuffer` (SDL
  `WIN_PollRawInput`); `GetRawInputData` is only imported by `toolframework2.dll` (tools). Fix:
  hook `GetRawInputData` and `GetRawInputBuffer` in every loaded module, walking `GetRawInputBuffer`
  batches record by record (`raw_buffer_records`, 8-byte `NEXTRAWINPUTBLOCK` alignment; tests in
  `input/tests/rawinput.rs`). The DLL logs the patched-module counts to the Proton log.
- What worked: automatic injection by the host, Ctrl+Tab reaching the host, widgets visible.

**In-game run 2 (2026-10-21, KDE Wayland, Proton 11.0) — all PASS.** Layer reinstalled, hook with
`GetRawInputBuffer`. Started Deadlock with the test-mode launch options only; the host injected the
hook automatically when the layer connected. P1 instant dim + cursor; P2 W, mouse-look and shooting
blocked in edit mode; P3 10/10 clicks on the button and typing lands in the input; P4 drag works;
P5 ~20 cycles, movement/mouse-look immediately back, not crouched, no repeats — including W pressed
in edit mode then exit, the case that left the game without input in run 1 (so that was the stale
layer's C2 running alongside, not a hook bug); P6 never minimized, paused or lost the mouse.

**Risks (for the hardware run).**
- **Keyboard assumed legacy `WM_KEY*`.** The WndProc gate catches keyboard; a game using *raw*
  keyboard (`RIDEV_NOLEGACY` on the keyboard) would bypass it. Deadlock is expected to use legacy
  `WM_KEY*` for UI/text, but confirm Ctrl+Tab and typing register. If not, the raw hook must extend
  to keyboard.
- **IAT hook vs. dynamic resolution.** The hook patches the game's import table for
  `GetRawInputData`. If Source 2 resolves it via `GetProcAddress` instead of a static import, the
  IAT patch misses it and an inline detour (MinHook/retour) is needed. Confirm the camera actually
  stops in edit mode (P2) first.
- **DLL load method under Proton:** runtime injection from outside Steam's container is proven
  against a plain Wine session, not yet against Deadlock's container (see above).
- **Proton-version fragility.** Depends on Wine delivering keyboard as `WM_KEY*`, `SetWindowLongPtrW`
  subclassing working in the container, and user32 raw-input semantics. Record the Proton version
  tested and re-check on upgrades.

## Mechanism chosen

- **Hotkey and capture: C3.** The hook owns Ctrl+Tab itself (rising edge, `WndProc` keyboard), so no
  separate H candidate is needed. Keyboard: `WndProc` subclass of the game window. Mouse: IAT hooks
  of `GetRawInputData`/`GetRawInputBuffer` in every loaded module (Deadlock: `SDL3.dll`). Same
  mechanism as the Windows plan.
- **Loading: runtime injection** (`spikes/inject`): host → `uw-spike-inject` (Linux) → Proton's
  `wine` joins the game's session → `uw-spike-inject-win.exe` → remote `LoadLibraryW`.
- **Transport:** hook → host over localhost TCP; host → layer over the Unix socket (cursor, edit mode).

**Packaging implications.** No group membership, no root, no launch option and no files in the
game folder or prefix. Underwolf ships three extra artifacts for Linux: the hook DLL and the
injector `.exe` (cross-built with mingw), and the Linux launcher. The Vulkan layer still installs
as a user implicit layer. The spike's `UW_SPIKE_*` launch options only select test modes; the
product needs a runtime switch instead.

**Dependence on Wine/Proton internals (break risk across Proton versions).**
- Joining the session from outside: depends on Wine's wineserver socket layout
  (`/tmp/.wine-<uid>/server-<dev>-<ino>`), the pressure-vessel container sharing `/tmp` and the
  PID namespace with the host, and copying the right `WINE*SYNC` settings. Medium risk; the
  launcher refuses to run (rather than misbehave) if the session can't be found.
- Injection itself: standard Win32 (`VirtualAllocEx`/`WriteProcessMemory`/`CreateRemoteThread`),
  well supported by Wine. Low risk.
- The hook: `WndProc` subclassing, `WM_KEY*` keyboard delivery, and SDL3 reading the mouse through
  statically imported `GetRawInputBuffer`. Low risk from Proton; moderate risk from game updates
  (an SDL change to dynamic resolution or raw keyboard would need an inline detour / raw keyboard
  handling).

## Risks

Per-candidate risks are listed above. For C3: keyboard assumed legacy `WM_KEY*` (a game enabling
SDL raw keyboard would bypass the gate); raw-input functions assumed statically imported; joining
the game's Wine session from outside depends on container/wineserver details (see above); VAC
(spec §3: injection is what Overwolf/Discord/OBS do, input path only). Known gap: if the host
restarts while the game runs, the hook's sender has exited and re-injection is a no-op — the hook
must reconnect in milestone 1.

## Not tested and why

- **The `cfg(windows)` hook in CI**: needs a game window and real input; covered by the owner's
  in-game runs. Injection itself is covered by `spikes/inject/tests/wine_e2e.rs` (ignored; needs Proton).
- **Other Proton versions** (only 11.0) and **Proton's Wayland driver** (`winewayland`; the game ran
  through XWayland in both cells).
- **AMD/Mesa**: only the NVIDIA machine is available (shared with Spike 2).
- C1 on GNOME (C1 already fails P6 on KDE); C2 on GNOME (C2 leaks the mouse on KDE).

## Decisions needed from owner

1. ~~DLL load method under Proton~~ — decided: runtime injection, exactly as Overwolf does, with
   no game-side setup (see "How the DLL is loaded").
   Parallel to the reader's access-method question (Spike 3).
2. ~~Transport~~ — owner: "do whatever Overwolf does". How Overwolf's in-game module talks to its
   client isn't publicly documented, and the spec rules out reverse engineering the client, so it
   can't be copied. Decided (assistant's call under that instruction): **keep localhost TCP**. It
   works the same on Windows and under Proton. A Windows named pipe or shared memory would stay
   inside Wine, where the Linux host can't reach it.
3. ~~Accept C3's input-path surface~~ — owner: "do whatever Overwolf does". Overwolf's injected
   module captures game input in its exclusive mode (spec §6.3 uses the same model), so **accepted**:
   `WndProc` subclass, `GetRawInputData`/`GetRawInputBuffer`/`RegisterRawInputDevices` IAT hooks and
   tagged `SendInput` releases. Input path only: no game memory, no game logic.
