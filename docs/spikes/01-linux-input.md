# Spike 1: Linux input under Proton

## Question

> Prove the overlay can capture mouse/keyboard in edit mode and pass it back to Deadlock
> afterwards, on KDE and GNOME Wayland sessions (the game itself runs through XWayland).
> (Session matrix per plan Amendment B.)

## Verdict

In progress.

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
| KDE Wayland | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING | |
| GNOME Wayland | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING | |

**Build and load (for the hardware run).**
1. Install the mingw linker: `sudo pacman -S mingw-w64-gcc` (the `x86_64-pc-windows-gnu` Rust target
   is installed, but linking the DLL needs mingw; without it the DLL only type-checks).
2. `cargo build --release --target x86_64-pc-windows-gnu -p uw-spike-input` →
   `target/x86_64-pc-windows-gnu/release/uw_spike_input.dll`.
3. Load it into the game under Proton (owner decision — record which works through the container):
   **DLL proxy/hijack** (ship it as a proxy for a DLL the game loads, e.g. `dxgi.dll`/`version.dll`,
   next to the game binary, forwarding the real exports — how ReShade/SpecialK load under Proton),
   or **`WINEDLLOVERRIDES`** in the Steam launch options.
4. Start the host in C3 mode: `cd spikes/osr-host && npm start -- --capture win32 --widget input --widget static`.
5. Launch Deadlock with `UW_SPIKE_MODE=composite UW_SPIKE_CAPTURE=win32 %command%`, enter the
   Hideout, and fill in the matrix.

**Risks (for the hardware run).**
- **Keyboard assumed legacy `WM_KEY*`.** The WndProc gate catches keyboard; a game using *raw*
  keyboard (`RIDEV_NOLEGACY` on the keyboard) would bypass it. Deadlock is expected to use legacy
  `WM_KEY*` for UI/text, but confirm Ctrl+Tab and typing register. If not, the raw hook must extend
  to keyboard.
- **IAT hook vs. dynamic resolution.** The hook patches the game's import table for
  `GetRawInputData`. If Source 2 resolves it via `GetProcAddress` instead of a static import, the
  IAT patch misses it and an inline detour (MinHook/retour) is needed. Confirm the camera actually
  stops in edit mode (P2) first.
- **DLL load method under Proton** (proxy vs. override) is unverified.
- **Proton-version fragility.** Depends on Wine delivering keyboard as `WM_KEY*`, `SetWindowLongPtrW`
  subclassing working in the container, and user32 raw-input semantics. Record the Proton version
  tested and re-check on upgrades.

## Mechanism chosen

Pending the C3 hardware run. If C3 passes all cells it is the chosen capture mechanism (it also
matches the Windows plan); its hotkey is self-contained, so no separate H candidate is needed.

## Risks

Per-candidate risks are listed above. For the chosen C3 path the top risks are the raw-input
assumptions (keyboard via `WM_KEY*`; `GetRawInputData` statically imported) and the DLL load method
under Proton — all resolved by the hardware run.

## Not tested and why

- **The whole C3 P1–P6 matrix** on both sessions: needs Deadlock under Proton on the test machine
  and a GNOME session; the `cfg(windows)` hook can't run in CI (no Wine/Proton, and the DLL needs
  mingw to link). Owner runs it.
- **AMD/Mesa**: only the NVIDIA machine is available (shared with Spike 2).
- C1 on GNOME (C1 already fails P6 on KDE); C2 on GNOME (C2 leaks the mouse on KDE).

## Decisions needed from owner

1. **DLL load method under Proton** — DLL proxy/hijack (e.g. `dxgi.dll` next to the game) vs.
   `WINEDLLOVERRIDES`. Pick after the hardware run shows which loads reliably in the container.
   Parallel to the reader's access-method question (Spike 3).
2. **Transport** — keep localhost TCP for the hook→host path, or unify on the Unix socket if AF_UNIX
   turns out reachable from inside Wine.
3. **Accept C3's input-path surface** — it hooks `GetRawInputData`/`RegisterRawInputDevices` and uses
   `SendInput` for held-button release. All input-path-only (no game memory, no game logic), but more
   Wine-internal surface and more Proton-version risk than a pure WndProc approach.
