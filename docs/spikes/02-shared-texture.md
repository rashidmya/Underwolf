# Spike 2: Shared-texture compositing

## Question

> Prove an Electron offscreen-rendered texture can be composited by (a) the Vulkan layer under
> Proton and on Windows, and (b) the DX11 hook, within the performance target.

## Verdict

In progress (Linux). Interim result: **PARTIAL**. The Vulkan layer path works in Deadlock under
Proton within budget. Electron's GPU shared textures are empty on NVIDIA, so on the owner's
decision widgets go through a CPU copy (see Owner decisions).

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

## Owner decisions

- 2026-10-20: **Allow a CPU copy for widget frames.** Electron renders offscreen to a CPU bitmap;
  the layer uploads it to the GPU. This overrides "textures must stay on the GPU" for widget
  frames. Its cost is measured in Task 5.

## Not tested and why

- AMD/Mesa: no AMD hardware available. Required before the milestone 1 release.
- Windows: Tasks 14–15.
