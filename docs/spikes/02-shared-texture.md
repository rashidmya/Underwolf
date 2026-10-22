# Spike 2: Shared-texture compositing

## Question

> Prove an Electron offscreen-rendered texture can be composited by (a) the Vulkan layer under
> Proton and on Windows, and (b) the DX11 hook, within the performance target.

## Verdict

**Linux: GO, with owner-accepted conditions.** Windows (a: Vulkan layer, b: DX11 hook) is not
scheduled (Tasks 14–15 dropped, Amendment C).

- Electron widgets are composited inside Deadlock's frames under Proton by the Vulkan layer.
  They survive host restarts, game restarts, alt-tab, windowed ↔ fullscreen and resolution
  changes.
- Condition 1: Electron's GPU shared textures are empty on NVIDIA, so widget frames take a CPU
  copy. The owner accepted this (Amendment A).
- Condition 2: the full overlay cost (Chromium + copy + layer) is about 3% in a CPU-bound game,
  just over budget on one measure per benchmark. The owner accepted it for the spike;
  optimisation goes into milestone 1.

## Evidence

### Test machine

CachyOS, KDE Plasma (Wayland), RTX 4090 with NVIDIA proprietary 615.78, monitors 2560×1440 @ 144 Hz
and 360 Hz. Deadlock (app 1422450) on Proton 11.0 with Steam Linux Runtime 4 (pressure-vessel).

### Layer loads inside Proton's container (Task 2)

- The user-level implicit layer manifest (`~/.local/share/vulkan/implicit_layer.d/`) is picked up
  inside pressure-vessel. The game process maps `libuw_spike_layer.so`, and
  `~/steam-1422450.log` shows `Insert instance layer "VK_LAYER_UNDERWOLF_spike"`.
- Under Proton the game's argv0 is `S:\steamapps\common\Deadlock\game\bin\win64\deadlock.exe`.
  The target check matches it. In non-target processes (vkcube, Electron's GPU process) the
  layer stays passthrough and writes nothing.
- No glibc or container-path problems.

### Layer draws inside the game frame (Task 3)

- A translucent 400×300 rectangle at (100, 100) is visible in the Hideout, over the game and
  under the cursor (owner confirmed).
- It survives alt-tab, windowed ↔ fullscreen and in-game resolution changes (owner confirmed).
- vkcube with the Khronos validation layer below ours: 0 validation errors.

### Performance: passthrough vs test rectangle (Deadlock Hideout, 3 × 60 s per mode, alternating)

| Run | Passthrough | Rectangle | FPS loss | p99 change | Valid? |
|---|---|---|---|---|---|
| 1 | 44.86 fps | 44.70 fps | +0.35% | −1.23% | No: game unfocused, background frame cap |
| 2 | 392.36 fps | 392.42 fps | −0.02% | +0.05% | No: in-game `fps_max` cap (~392 fps) |
| 3 | 421.88 fps, p99 3.592 ms | 421.82 fps, p99 3.577 ms | **+0.02%** | **−0.39%** | Yes (`fps_max 0`, focused) |

Run 3's per-run results are all 421.5–422.8 fps, which suggests the Hideout is CPU-bound at 1440p
on this GPU. That's realistic for this machine, but it hides the overlay's GPU cost. The widget
benchmark (Task 5) also runs a GPU-bound scenario.

### Electron GPU shared textures on NVIDIA: blocked (Task 4)

- **Default settings:** Electron 44 delivers **no** shared-texture frames.
  - Wayland: "Unable to initialize SkSurface".
  - X11: the GPU sandbox blocks loading GBM's DRI backend.
  - X11 with the sandbox off: "OzoneImageBacking::ProduceSkiaGanesh failed to create GL
    representation". Chromium's GL path can't render into GBM buffers on the NVIDIA driver.
- **With `--ozone-platform=x11 --disable-gpu-sandbox --use-angle=vulkan --enable-features=Vulkan`**
  (each verified necessary), frames arrive (BGRA, 1 plane, modifier 0 = LINEAR, ~56 fps).
  However, **every frame is empty (all-zero pixels)**:
  - Our dmabuf import reads exact pixels on this GPU (`cargo test -p uw-spike-dmabuf --test gpu_roundtrip`).
  - Electron's CPU offscreen path renders the same page correctly with the same flags.
  - Zero pixels in 225/225 frames through both the Vulkan import and a CPU mmap. The same holds
    for Wayland + ANGLE-Vulkan, Skia Graphite, an opaque window, and Electron 45.0.0-beta.5.
    Waiting 50 ms before reading changes nothing, and the GPU process logs no errors.
  - Electron's own shared-texture tests only run on macOS arm64, so the Linux path is untested
    upstream.

### Widget compositing in Deadlock (Task 5, CPU-copy path)

- **Pipeline:** Electron CPU offscreen rendering (default Chromium switches, GPU sandbox on) →
  `NativeImage.toBitmap()` (BGRA, premultiplied) → copy into one of 3 shared-memory slots per
  widget (memfd) → socket message → layer imports the slot pages as a `VkBuffer`
  (`VK_EXT_external_memory_host`, alignment 0x1000) → `vkCmdCopyBufferToImage` into a
  layer-owned widget image → textured quad with premultiplied blending → ack after the
  submission's fence. CPU copies: `toBitmap` plus the slot write, both on the host. The layer
  does no CPU copies.
- **Correctness:** the owner confirmed three widgets in the Hideout (animated plus two static)
  with correct colours and clean edges on Deadlock's `B8G8R8A8_SRGB` 2560×1440 swapchain (sRGB
  widget views). vkcube with core + synchronization validation: 0 messages, including stress
  with 7 host connect/disconnect cycles.
- **Robustness:**
  - Host stop and restart while the game runs: widgets disappear and come back (owner
    confirmed).
  - Quitting the game while the host runs: the host carries on (4 timeouts at exit, no frame-pool
    warnings).
  - Alt-tab, windowed ↔ fullscreen and resolution changes with widgets showing: widgets stay in
    place (owner confirmed).
  - 2 × 12 host restart cycles at heavy settings: no crash.
- Host CPU time for `toBitmap`: 0.15–0.19 ms per widget frame. Acks and drops: e.g. 25,108
  acked, 28 dropped, 0 timeouts over a 7½-minute run.

#### Bugs found and fixed in-game

1. **GPU hang (`VK_ERROR_DEVICE_LOST`, NVRM Xid 109 "CTX SWITCH TIMEOUT") when the host
   reconnected.**
   - Cause: the layer called `vkDeviceWaitIdle` from inside the game's `vkQueuePresentKHR`. That
     call requires exclusive access to all of the game's queues, and Deadlock submits from other
     threads.
   - Fix: the layer never waits on the whole device. Retired objects go to graveyards and are
     freed once the layer's own per-frame fences show completion. Descriptor sets are never
     rewritten.
   - vkcube (single-threaded) couldn't reproduce it. Confirmed fixed in-game: 24 reconnects
     plus 3 benchmark restarts at heavy settings with no crash.
   - **Lesson for milestone 1: the layer must never do device-wide waits.**
2. **A widget showed another widget's pixels after a host restart.**
   - Cause: the overlay checked whether a descriptor set was current by comparing `VkImageView`
     handles, and the driver reuses handle values after destruction.
   - Fix: key descriptor sets on a per-image serial.
   - Validation-clean in vkcube; the in-game visual re-check is still to do.
3. **Static widgets never appeared.**
   - Cause: a static page renders once, and that frame was dropped before the layer connected.
   - Fix: the layer keeps the latest frame per widget outside composite mode, and the host
     repaints all widgets when a new layer connects.

### Performance: full overlay vs nothing (Deadlock Hideout, 3 × 60 s per mode, alternating)

"Full" means: A = layer in passthrough with **no host running**; B = host running three widgets
(one 400×300 animated at 60 fps, two 300×200 static) composited by the layer.

| Benchmark | A (off) | B (widgets) | FPS loss | p99 change | Gate |
|---|---|---|---|---|---|
| 1: normal settings | 450.58 fps, p99 3.380 ms | 438.25 fps, p99 3.501 ms | +2.74% | **+3.57%** | FAIL on p99 |
| Layer only: host running in both phases | 443.49 fps, p99 3.468 ms | 437.76 fps, p99 3.510 ms | +1.29% | +1.22% | PASS |
| 2: owner's max settings, DLSS off | 413.90 fps, p99 3.410 ms | 401.18 fps, p99 3.495 ms | **+3.07%** | +2.50% | FAIL on FPS |

- About half the cost is the layer (upload and draw) and half is Chromium rendering plus the
  copies, competing with the game for CPU.
- Even at maximum settings the game ran at about 410 fps on the RTX 4090, so it stayed
  CPU-bound. **A GPU-bound measurement wasn't achievable on this machine**, so the GPU share of
  the cost is under-represented.
- An earlier run of benchmark 2 was invalid because the game crashed (bug 1).

## Mechanism chosen (Linux)

- Vulkan implicit layer installed per user (`~/.local/share/vulkan/implicit_layer.d`), active
  only for `deadlock.exe`.
- Draws inside the game's present with its own render pass, blending premultiplied.
- Widgets: Electron CPU offscreen rendering → memfd slots → host-pointer import → GPU copy →
  textured quads.

## Risks

- **CPU cost:** Chromium rendering competes with CPU-bound games. Optimisations for milestone 1:
  send only the changed region of each frame; per-widget frame rates (static widgets already send
  nothing until they change); use the GPU path where it works.
- **Driver variance:** `VK_EXT_external_memory_host` behaviour (it needed a read-write mapping on
  NVIDIA) and Chromium's GPU shared-texture path both depend on the driver. The NVIDIA blank-texture
  issue could be fixed upstream later, re-enabling a zero-copy path; the dmabuf code is kept.
- **Single host connection:** the host serves one layer at a time. Two target processes (for
  example vkcube and Deadlock) knock each other off. Production needs multi-client support.
- **Memory:** slot mappings in the layer are never unmapped (a small leak per host restart).

## Owner decisions

- 2026-10-20: **Allow a CPU copy for widget frames.** Electron renders offscreen to a CPU bitmap;
  the layer uploads it to the GPU. This overrides "textures must stay on the GPU" for widget
  frames. Its cost is measured in Task 5.
- 2026-10-20: **Accept the ~3% full-overlay cost for the spike** (benchmark 1 p99 +3.57%,
  benchmark 2 FPS −3.07%); optimise in milestone 1.

## Decisions needed from owner

- None open for Linux. For milestone 1: whether the ~3% budget applies to the layer alone or to
  the whole overlay including Chromium, and which optimisations come first.

## Not tested and why

- AMD/Mesa: no AMD hardware available. Required before the milestone 1 release, including
  whether Electron's GPU shared textures work there (they would remove the CPU copy).
- A GPU-bound benchmark: the RTX 4090 kept Deadlock CPU-bound even at maximum settings.
- GNOME, and X11 sessions: compositing happens inside Vulkan and should be session-independent,
  but it was only run on KDE Wayland. Spike 1 covers the session matrix.
- Windows: not scheduled (Tasks 14–15 dropped, Amendment C).
