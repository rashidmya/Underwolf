# Underwolf Technical Spikes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Answer the three §8 spike questions (Linux input under Proton, shared-texture compositing, Deadlock live data via read-only memory reading) with working prototypes and measured go/no-go findings. The milestone plans will be written from these findings.

**Architecture:** One Cargo workspace with throwaway-quality but tested spike crates under `spikes/`: a Vulkan implicit layer (draws inside the game frame), an Electron offscreen-rendering host with a napi-rs bridge that passes shared textures to the layer over a Unix socket, an input harness, and an external read-only Deadlock memory reader. Each spike ends in a findings doc under `docs/spikes/`.

**Tech Stack:** Rust 1.96 (`ash`, `napi-rs` 3.x, `rustix` or `nix` for SCM_RIGHTS, `serde` + `postcard`, `windows` crate on Windows), Electron 44.x + TypeScript, Node 24.

**Spec:** `docs/superpowers/specs/2026-10-20-underwolf-design.md` (approved as-is, 2026-10-20)

## Global Constraints

- "Underwolf must never write to game memory or alter game behavior. Anything inside the game process is read/draw-only."
- "never write to game memory, never inject code for data collection, never alter game behavior."
- "No reverse engineering of the Overwolf client, no redistribution of Overwolf code or apps."
- "If a spike fails, stop and bring the finding back to the owner before re-planning. Do not quietly add a windowed fallback or a memory-writing workaround."
- "If the Deadlock provider can't get a value, the event/field is simply not emitted. No fabricated or estimated values."
- Performance gate: "overlay costs under ~2–3% FPS; no visible stutter". This plan measures it as: average FPS loss ≤ 3.0% **and** p99 frame-time increase ≤ 3.0%, using the bench procedure in `spikes/README.md`.
- "Textures must stay on the GPU (no CPU copies)". GPU-to-GPU copies are allowed.
- Electron 44.x (current 44.9.1; at least 39 is needed for the `textureInfo.handle` structure). Rust toolchain pinned to 1.96.
- Supported environments: NVIDIA (proprietary) and AMD (Mesa) GPUs; KDE Plasma and GNOME on X11 and Wayland sessions; Windows 10 and 11.
- License: MIT.
- Overlay testing in Deadlock happens in the Hideout or bot matches. Real matches are only for the read-only reader (Task 12); the owner has accepted that VAC risk.
- Linux test machine: CachyOS, KDE Wayland, RTX 4090 (driver 615.78), `kernel.yama.ptrace_scope = 1`, Deadlock app ID `1422450`, Proton 10.0.

## Review Focus

1. **The layer loads into non-target Vulkan processes** (Electron's own GPU process, Steam, other games). It must stay pure passthrough there, with no socket, no drawing and no stats file. Tests: `is_target_cmdline` (Task 1), plus the "Electron and vkcube with the layer installed" check (Task 2, Step 6).
2. **The layer is absent, disconnects, or the game exits mid-frame.** Electron must release every texture within 100 ms and keep rendering, never draining its frame pool. Test: `send_frame_times_out_without_peer` and `send_frame_drops_after_peer_disconnect` (Task 4).
3. **Stuck keys on leaving edit mode.** Ctrl is crouch in Deadlock; after exiting with Ctrl+Tab the hero must not be crouched, and W/A/S/D must not repeat. Test: criterion P5 in Task 7's matrix, run 20 times per cell.
4. **Swapchain recreation, and a swapchain extent that differs from the window size** (alt-tab, resolution change, render scaling). The overlay must keep drawing at the right place and clicks must land on what's under the cursor. Tests: `maps_window_to_swapchain_coords` (Task 7) and the resolution-change checks (Task 3, Step 5; Task 5, Step 6).
5. **A game update moves memory layouts.** The reader must stop emitting rather than emit wrong values. Tests: `emitter_goes_silent_after_invalid_snapshot` (Task 11) and the forced-corruption run (Task 12, Step 3).

## Order and parallelism

Spike 2 (Linux) → Spike 1 (Linux). Spike 1 reuses Spike 2's pipeline to deliver input to the widgets. Spike 3 (Tasks 9–13) is independent and can run in parallel with Tasks 2–8. The Windows tasks (14–16) need a Windows 10/11 machine with Deadlock. Task 17 closes the spikes.

---

### Task 1: Workspace, safety guard, frame-time stats

**Files:**
- Create: `Cargo.toml` (workspace: `members = ["spikes/*"]`, `exclude = ["spikes/osr-host"]`), `rust-toolchain.toml` (`channel = "1.96"`), `.gitignore` (`target/`, `node_modules/`, `*.csv`, `*.jsonl`), `LICENSE` (MIT, holder: project owner)
- Create: `spikes/README.md`: safety rules (copy the Global Constraints), how to run each spike, the bench procedure (below)
- Create: `spikes/common/src/{lib.rs,target.rs,stats.rs}`, `spikes/common/src/bin/uw-spike-bench.rs`
- Test: `spikes/common/tests/{target.rs,stats.rs}`

**Interfaces:**
- Produces: `uw_spike_common::target::is_target_cmdline(argv0: &str) -> bool`; `uw_spike_common::stats::{FrameStats, Summary { avg_fps: f64, p99_ms: f64, frames: usize }, Comparison { fps_loss_pct: f64, p99_increase_pct: f64 }, compare(base: &Summary, test: &Summary) -> Comparison, within_budget(c: &Comparison) -> bool}`; `FrameStats::push_interval_ns(&mut self, ns: u64)`, `FrameStats::summary(&self, skip_first_secs: f64) -> Summary`, `FrameStats::from_csv(path) -> io::Result<FrameStats>`; CSV format is one line per frame, `present_ns,interval_ns`.
- Bench CLI: `uw-spike-bench compare <base.csv>... -- <test.csv>...` prints both summaries (mean over runs), the comparison, and `PASS`/`FAIL`, and exits 0/1.

**Bench procedure (put this in the README):** Deadlock Hideout, fixed spawn position and view, fixed graphics settings, VSync off, FPS cap off. Run 3 × 60 s per mode, alternating modes (A B A B A B). Drop the first 5 s of each run.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/target.rs
assert!(is_target_cmdline(r"C:\Program Files (x86)\Steam\steamapps\common\Deadlock\game\bin\win64\deadlock.exe"));
assert!(is_target_cmdline(r"Z:\home\u\.local\share\Steam\steamapps\common\Deadlock\game\bin\win64\DEADLOCK.EXE"));
assert!(!is_target_cmdline("/opt/electron/electron"));
assert!(!is_target_cmdline(r"C:\x\vconsole2.exe"));
assert!(!is_target_cmdline("deadlock.exe.bak"));
// tests/stats.rs
let mut s = FrameStats::default();
for _ in 0..98 { s.push_interval_ns(10_000_000) }
for _ in 0..2  { s.push_interval_ns(20_000_000) }
let sum = s.summary(0.0);
assert!((sum.avg_fps - 98.039).abs() < 0.01);   // mean interval 10.2 ms
assert_eq!(sum.p99_ms, 20.0);                     // nearest-rank, ceil(0.99*n)
let c = compare(&Summary{avg_fps:100.0,p99_ms:10.0,frames:1}, &Summary{avg_fps:97.0,p99_ms:10.3,frames:1});
assert!((c.fps_loss_pct - 3.0).abs() < 1e-9 && (c.p99_increase_pct - 3.0).abs() < 1e-9);
assert!(within_budget(&c));
assert!(!within_budget(&Comparison{fps_loss_pct:3.01, p99_increase_pct:0.0}));
```

Also `summary(5.0)` must drop frames whose cumulative time is ≤ 5 s (test: 1000 × 10 ms intervals → 500 frames remain).

- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-common` → compile errors (items not defined).
- [ ] **Step 3: Implement.** `is_target_cmdline` takes the basename after the last `\` or `/`, compares case-insensitively to `deadlock.exe`, and matches exactly. Write the bench CLI.
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-common` → all pass.
- [ ] **Step 5: Commit.** `git commit -m "spikes: workspace, target guard, frame-time stats"`

### Task 2: Vulkan implicit layer, passthrough + timing (Spike 2)

**Files:**
- Create: `spikes/vk-layer/` (crate `uw-spike-layer`, `crate-type = ["cdylib"]`, lib name `uw_spike_layer`): `src/lib.rs` (exports), `src/dispatch.rs` (instance/device dispatch tables keyed by dispatch pointer), `src/loader_abi.rs` (hand-written `vk_layer.h` structs: `VkLayerInstanceCreateInfo`, `VkLayerDeviceCreateInfo`, `VkLayerInstanceLink`, `VkLayerDeviceLink`, `VkNegotiateLayerInterface`; `ash` doesn't ship these), `src/present.rs`
- Create: `spikes/vk-layer/underwolf_spike_layer.json.in`, `spikes/vk-layer/install.sh`, `spikes/vk-layer/uninstall.sh`

**Interfaces:**
- Consumes: `is_target_cmdline`, `FrameStats` CSV format (Task 1).
- Produces: layer name `VK_LAYER_UNDERWOLF_spike`; exports `vkNegotiateLoaderLayerInterfaceVersion`, `vkGetInstanceProcAddr`, `vkGetDeviceProcAddr`. It intercepts `vkCreateInstance`, `vkDestroyInstance`, `vkCreateDevice`, `vkDestroyDevice`, `vkCreateSwapchainKHR`, `vkDestroySwapchainKHR`, `vkGetDeviceQueue`, `vkGetDeviceQueue2` and `vkQueuePresentKHR`. Env `UW_SPIKE_MODE` = `passthrough` (default) | `quad` | `composite`. The manifest has `"disable_environment": {"UW_SPIKE_LAYER_DISABLE": "1"}`. Stats go to `~/.cache/underwolf-spike/frames-<pid>-<mode>.csv`, written only when `is_target_cmdline(argv0)` is true or `UW_SPIKE_FORCE_TARGET=1` is set (needed for vkcube).
- `install.sh` builds a release, copies the `.so` to `~/.local/share/underwolf-spike/libuw_spike_layer.so`, and writes the manifest (absolute `library_path`) to `~/.local/share/vulkan/implicit_layer.d/underwolf_spike_layer.json`. Steam's own overlay layers already live in that directory, which shows that Proton's container picks it up.

- [ ] **Step 1: Write a failing test** `exports_resolve_hooks`: calling the exported `vkGetInstanceProcAddr(null, "vkCreateInstance")` returns a non-null pointer equal to the layer's hook, and `vkNegotiateLoaderLayerInterfaceVersion` with `loaderLayerInterfaceVersion = 2` returns `VK_SUCCESS` and fills in the layer's own GIPA.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-layer` → FAIL.
- [ ] **Step 3: Implement the passthrough layer.** Store dispatch by the loader's dispatch key (`*(void**)handle`). The present hook timestamps with `CLOCK_MONOTONIC`, pushes the interval, flushes the CSV every 1 s from a background thread, and never blocks the present path.
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-layer` → PASS.
- [ ] **Step 5: Verify in vkcube.** `./spikes/vk-layer/install.sh && VK_LOADER_DEBUG=layer UW_SPIKE_FORCE_TARGET=1 vkcube`. Expected: the loader log lists `VK_LAYER_UNDERWOLF_spike`, vkcube renders normally, and a CSV appears.
- [ ] **Step 6: Verify the non-target guard.** Run `vkcube` without `UW_SPIKE_FORCE_TARGET`, and run `npx electron --enable-features=Vulkan` with any page. Expected: both run normally and no new CSV appears.
- [ ] **Step 7: Verify in Deadlock.** Steam launch options: `PROTON_LOG=1 VK_LOADER_DEBUG=layer %command%`. Expected: `~/steam-1422450.log` shows the layer loaded inside the Proton container, the game is playable, and `frames-<pid>-passthrough.csv` exists. If the layer doesn't load (container path or glibc mismatch), record why in the Spike 2 notes before trying a fix.
- [ ] **Step 8: Commit.** `git commit -m "spikes: Vulkan implicit layer passthrough with frame timing"`

### Task 3: Layer draws a test quad (Spike 2)

**Files:**
- Create: `spikes/vk-layer/src/overlay.rs` (render pass, pipeline, per-swapchain-image framebuffers and command buffers), `spikes/vk-layer/shaders/quad.{vert,frag}` compiled to SPIR-V at build time (`build.rs` with `shaderc` or checked-in `.spv` plus the compile command in a comment)
- Modify: `spikes/vk-layer/src/present.rs`, `src/lib.rs`

**Interfaces:**
- Produces: `Overlay::draw(&mut self, queue: vk::Queue, image_index: u32, wait: &[vk::Semaphore], quads: &[Quad]) -> vk::Semaphore` (returns the semaphore that the real present waits on); `Quad { x: i32, y: i32, w: u32, h: u32, source: QuadSource }`; `QuadSource::Solid([f32; 4])` now, and `QuadSource::Image(vk::ImageView)` in Task 5. Blending uses premultiplied alpha (`ONE`, `ONE_MINUS_SRC_ALPHA`).

- [ ] **Step 1: Implement mode `quad`.** Draw a 400×300 quad at (100, 100) with color `[0.5, 0.0, 0.25, 0.5]` (premultiplied). In `vkCreateSwapchainKHR`, add `COLOR_ATTACHMENT` usage if it's missing. Use an image view in the swapchain format (handle `_SRGB` and `_UNORM`). Submit on the queue the app presents on, after checking that the queue family supports graphics; if it doesn't, log it and skip drawing, and never crash. Rebuild per-image resources on swapchain recreation.
- [ ] **Step 2: Verify in vkcube.** `UW_SPIKE_MODE=quad UW_SPIKE_FORCE_TARGET=1 vkcube`. Expected: a translucent quad over the spinning cube, with no validation errors when run with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`.
- [ ] **Step 3: Verify in Deadlock.** Launch options `UW_SPIKE_MODE=quad %command%`. Expected: the quad is visible in the Hideout, over the game but under the cursor.
- [ ] **Step 4: Measure.** Run the bench procedure, passthrough vs quad: `uw-spike-bench compare frames-*-passthrough.csv -- frames-*-quad.csv`. Record the numbers in `docs/spikes/02-shared-texture.md` (create it from the findings template in Task 6).
- [ ] **Step 5: Check swapchain recreation (Review Focus 4).** Alt-tab out and back, switch windowed ↔ fullscreen, change the resolution in-game. Expected: the quad stays at (100, 100) in swapchain pixels and there's no crash or hang.
- [ ] **Step 6: Commit.** `git commit -m "spikes: layer draws test quad over game frame"`

### Task 4: Electron OSR host, socket protocol, standalone import check (Spike 2)

**Files:**
- Create: `spikes/ipc/src/lib.rs` (crate `uw-spike-ipc`), test `spikes/ipc/tests/roundtrip.rs`
- Create: `spikes/osr-bridge/` (napi-rs crate `uw-spike-osr-bridge`, builds `osr_bridge.node`), test `spikes/osr-bridge/tests/bridge.rs`
- Create: `spikes/osr-host/{package.json,tsconfig.json,src/main.ts,widgets/solid.html,widgets/animated.html,widgets/static.html}`
- Create: `spikes/dmabuf-consumer/src/main.rs` (crate `uw-spike-consumer`)

**Interfaces:**
- Produces (ipc): socket path `~/.cache/underwolf-spike/overlay.sock` (a file socket, because the home directory is visible inside the Proton container). Each frame on the wire is a `u32` LE length followed by a `postcard` payload; fds travel via SCM_RIGHTS in the same `sendmsg`.
  ```rust
  pub enum Msg {
      Hello { protocol: u32, pid: u32 },                          // layer → host, protocol = 1
      Swapchain { width: u32, height: u32 },                      // layer → host, on create/recreate
      Frame { id: u64, widget: u32, x: i32, y: i32, width: u32, height: u32,
              drm_fourcc: u32, modifier: u64, planes: Vec<Plane> }, // host → layer, planes.len() fds attached
      Ack { id: u64 },                                            // layer → host, after GPU copy completed
      Layout { widget: u32, x: i32, y: i32, visible: bool },      // host → layer
  }
  pub struct Plane { pub offset: u32, pub stride: u32 }
  pub fn send(s: &UnixStream, m: &Msg, fds: &[BorrowedFd]) -> io::Result<()>;
  pub fn recv(s: &UnixStream) -> io::Result<(Msg, Vec<OwnedFd>)>;
  pub fn fourcc_for(pixel_format: &str) -> Option<u32>;   // "bgra" → DRM_FORMAT_ARGB8888, "rgba" → DRM_FORMAT_ABGR8888, else None
  pub fn parse_modifier(s: &str) -> Option<u64>;          // decimal string from Electron (u64 doesn't fit a JS number)
  ```
- Produces (bridge, JS API used by `main.ts`):
  ```ts
  startServer(socketPath: string): void
  sendFrame(f: { widget: number; x: number; y: number; width: number; height: number;
                 pixelFormat: string; modifier: string;
                 planes: { fd: number; stride: number; offset: number; size: number }[] }): Promise<'acked' | 'dropped' | 'timeout'>
  ```
  `sendFrame` returns `'dropped'` right away when no layer is connected, and `'timeout'` after 100 ms without an `Ack`. `main.ts` calls `texture.release()` after *any* result.
- Host: one offscreen `BrowserWindow` per widget (`show: false`, `transparent: true`, `webPreferences.offscreen.useSharedTexture: true`, `setFrameRate(60)`), loaded from `widgets/*.html`, positioned by a hard-coded layout. On `paint`, it maps `e.texture.textureInfo` (`handle.nativePixmap.planes`, `handle.nativePixmap.modifier`, `pixelFormat`, `codedSize`) to `sendFrame`. CLI flag `--consumer` points the host at the standalone consumer instead of a game.
- Consumer: `uw-spike-consumer <socket>` acts as the layer-side client and retries connecting for up to 10 s. It imports the first `Frame` via `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier`, copies it into its own image, copies that to a host-visible buffer (allowed here, because this is a test tool and not the frame path), prints the center pixel as `RRGGBBAA` hex, sends `Ack`, and exits 0.

- [ ] **Step 1: Write failing ipc tests.** `msg_roundtrip` (every variant survives `send`/`recv` over `UnixStream::pair()`), `fds_cross_socket` (send 2 fds of temp files; the received fds `fstat` to the same `st_ino`), `fourcc_mapping` (`"bgra"` → `0x34325241`, `"rgba"` → `0x34324241`, `"rgbaf16"` → `None`), `parse_modifier("72057594037927935") == Some(0x00ff_ffff_ffff_ffff)`.
- [ ] **Step 2: Write failing bridge tests (Review Focus 2).** `send_frame_drops_without_peer` returns `Dropped` in under 5 ms. `send_frame_times_out_without_peer` (peer connected but never acks) returns `Timeout` in 100–150 ms. `send_frame_drops_after_peer_disconnect`: the peer closes, the next call returns `Dropped`, and the server accepts a new peer afterwards. Test the Rust core directly (not through Node).
- [ ] **Step 3: Run, verify fail.** `cargo test -p uw-spike-ipc -p uw-spike-osr-bridge` → FAIL.
- [ ] **Step 4: Implement ipc and the bridge.**
- [ ] **Step 5: Run, verify pass.** Same command → PASS.
- [ ] **Step 6: Implement the host and consumer, then run the end-to-end import check.** `widgets/solid.html` has body background `rgb(255, 0, 128)`. Run: `cargo run -p uw-spike-consumer -- ~/.cache/underwolf-spike/overlay.sock & (cd spikes/osr-host && npm start -- --consumer --widget solid)`. Expected: the consumer prints `ff0080ff` and exits 0. If Electron reports a format other than `bgra`/`rgba`, or `handle.nativePixmap` is missing, stop and record it. That would be a Spike 2 blocker.
- [ ] **Step 7: Commit.** `git commit -m "spikes: Electron OSR shared-texture host, socket protocol, dmabuf import check"`

### Task 5: Layer composites Electron widgets in the game (Spike 2)

**Files:**
- Create: `spikes/vk-layer/src/client.rs` (background socket client), `spikes/vk-layer/src/import.rs`
- Modify: `spikes/vk-layer/src/{lib.rs,present.rs,overlay.rs}` (in `vkCreateDevice`, append `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier` and their dependencies when the physical device supports them; otherwise log and stay in passthrough)

**Interfaces:**
- Consumes: `uw_spike_ipc::{Msg, send, recv}`, `Overlay::draw` with `QuadSource::Image` (Task 3).
- Produces: in mode `composite`, the client thread connects to the socket (retrying every 1 s, never blocking present) and sends `Hello` plus `Swapchain`. For each `Frame`, the layer imports the dmabuf and records a GPU copy into a layer-owned per-widget image inside the next present's submission. It sends `Ack { id }` once that submission's fence signals. Each widget is drawn from its layer-owned image at its last `Frame`/`Layout` position.

- [ ] **Step 1: Implement import, copy, composite and ack.** The frame path must have no `vkMapMemory` or CPU readback (check by `grep -n MapMemory spikes/vk-layer/src` → only in absent/test code).
- [ ] **Step 2: Verify in vkcube.** `UW_SPIKE_MODE=composite UW_SPIKE_FORCE_TARGET=1 vkcube` plus `npm start -- --widget animated`. Expected: an animated widget over the cube, with transparent areas showing the cube. Edges of semi-transparent content must have no dark fringes; a dark fringe means a premultiplied-alpha mistake.
- [ ] **Step 3: Verify in Deadlock.** Start the host with `--widget animated --widget static --widget static`, then launch Deadlock with `UW_SPIKE_MODE=composite %command%`. Expected: three widgets inside the game frame in the Hideout.
- [ ] **Step 4: Check robustness.** Quit and restart the host while the game runs, and quit the game while the host runs. Expected: the widgets disappear and reappear, neither side crashes or hangs, and the host logs no `OSRSharedTextureNotReleased` warning.
- [ ] **Step 5: Measure.** Run the bench with the host running the 3-widget load (one 400×300 animated at 60 fps, two 300×200 static), passthrough vs composite: `uw-spike-bench compare ...`. Expected: `PASS`. Record the numbers.
- [ ] **Step 6: Check swapchain recreation (Review Focus 4).** Repeat Task 3, Step 5 with widgets on screen.
- [ ] **Step 7: Commit.** `git commit -m "spikes: composite Electron shared textures in Vulkan layer"`

### Task 6: Spike 2 (Linux) findings

**Files:**
- Create: `docs/spikes/TEMPLATE.md` with these sections: Question (verbatim from spec §8), Verdict (`GO` / `NO-GO` / `PARTIAL`), Evidence (numbers, matrices, logs), Mechanism chosen, Risks, Not tested and why, Decisions needed from owner
- Modify: `docs/spikes/02-shared-texture.md`

- [ ] **Step 1: Write the Linux part of the findings.** Include the passthrough/quad/composite numbers, whether the layer loaded in the Proton container, the extensions and modifiers seen, and the ack latency. Under "Not tested", list AMD/Mesa unless AMD hardware was available, and note that AMD is required before the milestone 1 release.
- [ ] **Step 2: Gate.** If the verdict isn't `GO`, stop and report to the owner (Global Constraints).
- [ ] **Step 3: Commit.** `git commit -m "docs: spike 2 Linux findings"`

### Task 7: Linux input under Proton (Spike 1)

**Files:**
- Modify: `spikes/ipc/src/lib.rs`: add `Msg::Input(InputEvent)` (owner of capture → host) and `Msg::EditMode { on: bool }` (either direction), plus
  ```rust
  pub enum InputEvent { MouseMove { x: f32, y: f32 }, MouseButton { button: u8, down: bool },
                        Wheel { dx: f32, dy: f32 }, Key { code: u32, down: bool, text: Option<String> } }
  pub fn window_to_swapchain(p: (f32, f32), window: (u32, u32), swapchain: (u32, u32)) -> (f32, f32);
  ```
- Create: `spikes/input/` (crate `uw-spike-input`): one module per candidate, `src/hotkey_{xi2,evdev,portal}.rs` and `src/capture_{surface,wine_x11,win32_hook}.rs`
- Modify: `spikes/vk-layer` (edit mode: 50% black dim over the game, outlined widgets, cursor sprite at the last `MouseMove`), `spikes/osr-host/src/main.ts` (on `Input`: hit-test widgets, drag a widget by its outline and send `Layout`, otherwise forward to `webContents.sendInputEvent` in widget-local coordinates)
- Create: `spikes/osr-host/widgets/input.html` (a button whose counter is visible, a text `<input>`)
- Test: `spikes/ipc/tests/coords.rs`

**Candidates.** Each gets a 2-day timebox. Run them in this order and stop at the first one that passes every required cell. Record each attempt, including failures.
- Hotkey (Ctrl+Tab while the game has focus): **H1** XInput2 raw key events read from a separate X connection opened by the layer inside the Wine process (read-only). **H2** evdev `/dev/input/event*` (needs `input` group membership). **H3** the XDG GlobalShortcuts portal.
- Capture during edit mode: **C1** an input-capturing fullscreen transparent surface raised over the game while edit mode is on (X11: override-redirect + pointer/keyboard grab; Wayland: compositor window), handing focus back on exit. **C2** filtering X events inside Wine's X11 driver in the game process, so the game never sees them and they are forwarded to the host. **C3** a Win32-side input hook in the game process under Proton, the same mechanism planned for Windows. C2 and C3 touch only the input path, never game memory or game logic.

**Pass criteria, per cell.** Matrix cells: KDE Wayland, KDE X11, GNOME Wayland, GNOME X11. All four are required (spec §7a). Installing a GNOME session on the test machine is a prerequisite.
- P1: Ctrl+Tab in-game enters edit mode in under 100 ms (dim visible).
- P2: In edit mode the game sees nothing: holding W doesn't move the hero and moving the mouse doesn't turn the camera.
- P3: The overlay cursor matches the visual position: 10 of 10 clicks hit the 20 px button, and typed text appears in the `<input>`.
- P4: A widget can be dragged to a new place.
- P5 (Review Focus 3): Ctrl+Tab exits; mouse-look and movement work right away; the hero isn't crouched; no key repeats. 20 enter/exit cycles with no failure.
- P6: The game doesn't minimize, pause or lose its mouse capture at any point.
- Record only, not a gate: whether a widget region can take clicks while the rest passes through to the game outside edit mode (needed later for `noPassThrough`).

- [ ] **Step 1: Write the failing test.** `maps_window_to_swapchain_coords`: `window_to_swapchain((960.0, 540.0), (1920, 1080), (2560, 1440)) == (1280.0, 720.0)` and `window_to_swapchain((0.0, 0.0), (1920,1080), (1280,720)) == (0.0, 0.0)`.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-ipc coords` → FAIL.
- [ ] **Step 3: Implement the ipc additions and edit mode in the layer and host.** Run `cargo test -p uw-spike-ipc` → PASS.
- [ ] **Step 4: Try the candidates in order** (H1+C1 first) in the Hideout. Fill in the per-cell matrix (P1–P6, pass/fail plus notes) in `docs/spikes/01-linux-input.md`.
- [ ] **Step 5: Commit after each candidate.** `git commit -m "spikes: input candidate <id> + results"`

### Task 8: Spike 1 findings

**Files:** Modify `docs/spikes/01-linux-input.md` (from the template)

- [ ] **Step 1: Write the findings.** Include the full matrix per candidate, the chosen hotkey and capture mechanisms, packaging implications (for example, H2 requires `input` group membership), and how much each mechanism depends on Wine/Proton internals and how likely it is to break with Proton versions.
- [ ] **Step 2: Gate.** If no combination passes all four cells, stop and report to the owner.
- [ ] **Step 3: Commit.** `git commit -m "docs: spike 1 findings"`

### Task 9: Deadlock reader, process discovery and read-only access (Spike 3)

**Files:**
- Create: `spikes/dl-reader/` (crate `uw-spike-dlreader`, binary `uw-dlreader`): `src/process.rs`, `src/mem.rs`, `src/main.rs`
- Test: `spikes/dl-reader/tests/{maps.rs,mem.rs}`, fixture `spikes/dl-reader/tests/fixtures/maps_proton.txt` (captured from a real Proton Deadlock process with `cat /proc/<pid>/maps`, with paths trimmed to the parts the parser needs)

**Interfaces:**
- Produces: `trait MemRead { fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<()>; }` with helpers `read_u32/u64/ptr/cstr(addr, max)`; `LinuxProcess::open(pid) -> io::Result<LinuxProcess>` (implements `MemRead` via `process_vm_readv`, read-only by construction, with no write API anywhere in the crate); `find_deadlock() -> Option<i32>` (scans `/proc/*/cmdline`, argv0 matched with `is_target_cmdline`); `parse_modules(maps: &str) -> Vec<Module { name: String, base: u64, size: u64 }>`, where `name` is the lowercase file basename, `base` is the lowest start and `size` = highest end − base.
- CLI: `uw-dlreader modules` prints the base addresses of `client.dll`, `engine2.dll` and `schemasystem.dll`, plus the access method used.

- [ ] **Step 1: Write failing tests.** `parses_proton_modules`: from the fixture, `client.dll`, `engine2.dll` and `schemasystem.dll` are present with the fixture's expected bases (fill them in from the captured file). `reads_own_memory`: `LinuxProcess::open(std::process::id())` reads a known static `[u8; 8]` back exactly. `read_unmapped_errors`: reading address `0x8` returns `Err`.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-dlreader` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, verify pass.** Same command → PASS.
- [ ] **Step 5: Check access under `ptrace_scope = 1`.** A plain `uw-dlreader modules` against the running game is expected to fail with `EPERM`. Try two methods: (a) `sudo setcap cap_sys_ptrace+ep target/release/uw-dlreader`; (b) a Steam launch-option wrapper `uw-dlreader wrap -- %command%`, which spawns the game as its descendant (the same way `gamemoderun %command%` sits in front of the command) and reads from inside the wrapper. Record which works through Proton's container and what each one means for the player (one-time root setup vs a launch option). Don't pick one; this decision goes to the owner.
- [ ] **Step 6: Commit.** `git commit -m "spikes: Deadlock process discovery and read-only memory access"`

### Task 10: Name-based offset resolution (Spike 3)

**Files:**
- Create: `spikes/dl-reader/src/{pattern.rs,pe.rs,interfaces.rs,schema.rs}`
- Test: `spikes/dl-reader/tests/{pattern.rs,pe.rs,schema_live.rs}` (`schema_live` is `#[ignore]` and needs the running game)

**Interfaces:**
- Consumes: `MemRead`, `Module` (Task 9).
- Produces: `Pattern::parse(&str) -> Result<Pattern, _>` (hex bytes and `?` wildcards); `scan(hay: &[u8], p: &Pattern) -> Option<usize>`; `rip_target(instr_addr: u64, disp: i32, instr_len: u32) -> u64`; `pe::exports(m: &dyn MemRead, base: u64) -> io::Result<HashMap<String, u64>>`; `interfaces::list(m, module) -> io::Result<HashMap<String, u64>>`, which follows `CreateInterface` to the module's interface registration list and walks `{create_fn, name, next}`; `schema::dump(m, schema_system: u64, scope: &str) -> io::Result<SchemaDump>`; `SchemaDump { build: String, classes: BTreeMap<String, BTreeMap<String, u32>> }` (class → field → offset), serde JSON.
- Approach: field offsets are resolved by class and field name through the game's own schema system, read from memory and never called, so they survive most game updates. Byte patterns are used only to locate roots (entity list, game rules) that the schema system doesn't expose. Each pattern is stored with the build it was written against. Struct layouts inside schemasystem come from public Source 2 tooling references; cite the reference in a code comment.
- CLI: `uw-dlreader dump-schema --out schema-<build>.json`, where build comes from `game/citadel/steam.inf` `ClientVersion`.

- [ ] **Step 1: Write failing tests.** `pattern_parse_and_scan`: `Pattern::parse("48 8B 05 ? ? ? ? C3")` scans `[0x90, 0x48, 0x8B, 0x05, 1, 2, 3, 4, 0xC3]` → `Some(1)`, and `parse("4G")` → `Err`. `rip_target(0x1000, 0x10, 7) == 0x1017`, `rip_target(0x1000, -0x10, 7) == 0xFF7`. `pe_exports_synthetic`: a synthetic in-memory PE image (built in the test with `IMAGE_EXPORT_DIRECTORY` at a known RVA, exporting `CreateInterface` at RVA `0x1234`) behind an in-memory `MemRead` gives `exports()["CreateInterface"] == base + 0x1234`. `schema_live` (ignored): the dump contains at least 1000 classes, and class names include a player-controller class and a game-rules class (assert by substring `PlayerController` and `GameRules`).
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-dlreader` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-dlreader`, then `cargo test -p uw-spike-dlreader -- --ignored` with the game running in the Hideout → PASS. Commit `schema-<build>.json` under `spikes/dl-reader/dumps/`.
- [ ] **Step 5: Commit.** `git commit -m "spikes: name-based offset resolution via Source 2 schema system"`

### Task 11: Feature probes and the Overwolf-shaped emitter (Spike 3)

**Files:**
- Create: `spikes/dl-reader/src/{snapshot.rs,emit.rs,validate.rs}`
- Test: `spikes/dl-reader/tests/emit.rs`

**Interfaces:**
- Consumes: `SchemaDump`, `MemRead`, patterns (Task 10).
- Produces:
  ```rust
  pub struct Snapshot { pub phase: Option<String>, pub game_mode: Option<String>, pub steam_id: Option<u64>,
      pub match_id: Option<u64>, pub clock_s: Option<f32>, pub paused: Option<bool>,
      pub team_score: Option<[i32; 2]>, pub players: Vec<Player>, pub towers_alive: Option<Vec<u32>> }
  pub struct Player { pub steam_id: Option<u64>, pub name: Option<String>, pub hero_id: Option<u32>,
      pub team: Option<u8>, pub kills: Option<u32>, pub deaths: Option<u32>, pub assists: Option<u32>,
      pub is_local: bool, pub items: Option<Vec<u32>> }
  pub enum Emission { Info { feature: String, category: String, key: String, value: serde_json::Value },
                      Event { name: String, data: serde_json::Value } }
  pub fn read_snapshot(m: &dyn MemRead, ctx: &ResolveCtx) -> Snapshot;   // any failed read → None for that field
  pub fn validate(s: &Snapshot, prev: Option<&Snapshot>) -> Result<(), Invalid>;
  pub struct Emitter { /* last snapshot, silenced flag */ }
  impl Emitter { pub fn step(&mut self, s: Snapshot) -> Vec<Emission>; pub fn silenced(&self) -> bool; }
  ```
  Feature and key names come verbatim from spec §6.4. Exact Overwolf value formats are deferred to the provider plan; the spike checks only that the data is available and correct.
- `validate` invariants: at most 12 players; local player found exactly once when `players` is non-empty; `team` values inside the set discovered in Task 10's schema/entity probing; kills/deaths/assists never decrease within one `match_id`; `clock_s` never decreases unless `paused` became true or `match_id` changed.
- `Emitter::step` behavior: the first invalid snapshot silences the emitter permanently for the process lifetime and emits nothing more (fail-safe). Info is emitted only for fields that changed and are `Some`. Events come from diffs: local kills +1 → `kill`; local deaths +1 → `death`; local assists +1 → `assist`; phase changing into in-match → `match_start` and out → `match_end`; `paused` false→true → `game_paused`; a tower id disappearing from `towers_alive` → `tower_destroyed`; `match_clock` once per whole second of `clock_s`.
- CLI: `uw-dlreader watch --hz 20 --jsonl out.jsonl` writes one emission per line with a `t_ms` timestamp, and prints a feature-status table on exit (`supported` / `unavailable` / `silenced`).

- [ ] **Step 1: Write failing tests.** `kill_delta_emits_kill_event`. `none_field_emits_nothing` (a snapshot with `match_id: None` yields no `match_id` Info). `phase_transition_emits_match_start_and_end`. `pause_emits_game_paused_once`. `tower_removal_emits_tower_destroyed`. `validate_rejects_13_players`. `validate_rejects_decreasing_kills`. `emitter_goes_silent_after_invalid_snapshot`: after one invalid snapshot, a later valid snapshot with a kill delta returns `vec![]` and `silenced() == true`.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-dlreader emit` → FAIL.
- [ ] **Step 3: Implement `validate` and `Emitter`, then `read_snapshot`.** Find the class and field for each `Snapshot` field in `schema-<build>.json`, and record each mapping (feature → class.field or pattern) in a table in `docs/spikes/03-deadlock-memory.md` as you go. A field that can't be found stays `None`; never derive or estimate it.
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-dlreader` → PASS.
- [ ] **Step 5: Commit.** `git commit -m "spikes: Deadlock snapshot, validator, Overwolf-shaped emitter"`

### Task 12: Accuracy, latency and robustness runs (Spike 3)

**Files:**
- Create: `spikes/dl-reader/src/verify.rs`, plus a `uw-dlreader verify --jsonl out.jsonl --match-id <id>` subcommand. It fetches `https://api.deadlock-api.com/v1/matches/{match_id}/metadata` and compares final per-player kills/deaths/assists, hero and team, match outcome and match ID, printing a per-field `OK`/`MISMATCH`/`MISSING` table.
- Modify: `docs/spikes/03-deadlock-memory.md`

- [ ] **Step 1: Run accuracy checks.** Record `watch` through at least 3 real matches (read-only reader only, no overlay layer installed) and 1 bot match. Run `verify` on the real matches. Expected: no `MISMATCH`. Check the bot match by hand against the end-of-match scoreboard.
- [ ] **Step 2: Check latency.** During the bot match, screen-record alongside `watch`, and for 5 kills compare the kill-feed time with the `kill` event's `t_ms`. Expected: ≤ 1 s each.
- [ ] **Step 3: Check fail-safe (Review Focus 5).** Run `UW_DLREADER_CORRUPT=<Class>.<field> uw-dlreader watch`, which adds 8 to that offset after resolution, once each for the kills field and the team field. Expected: the emitter goes silent and the status table shows `silenced`, with no wrong values in the JSONL before that.
- [ ] **Step 4: Check robustness across updates.** After the next Deadlock patch, rerun `watch` with **no code changes**. Record which features survived, which patterns broke, and the schema-dump diff (`diff` of the two `schema-<build>.json` files). If no patch lands within the spike window, use Steam's `download_depot` to get the previous build's `client.dll` and run the pattern scans against it statically, and record that the schema side wasn't tested.
- [ ] **Step 5: Commit.** `git commit -m "spikes: Deadlock reader accuracy and robustness results"`

### Task 13: Spike 3 (Linux) findings

**Files:** Modify `docs/spikes/03-deadlock-memory.md` (from the template)

- [ ] **Step 1: Write the findings, including the feature-by-source table** that spec §8.3 asks for. Columns: Feature/key | Source (memory / console log / deadlock-api.com / unavailable) | Resolution (schema name / pattern) | Accuracy result | Survived patch | Notes. Add one row per §6.4 feature and event. For each feature that memory doesn't provide, name the supplementary source the spec allows (console log, local state files, deadlock-api.com for `match_history` and post-match), or mark it `unavailable`. Include the access-method decision for the owner (Task 9, Step 5).
- [ ] **Step 2: Gate.** If live features (`phase`, `roster`, `kill`/`death`/`assist`, `match_clock`) aren't readable, stop and report to the owner.
- [ ] **Step 3: Commit.** `git commit -m "docs: spike 3 Linux findings"`

### Task 14: Windows: Vulkan layer and Electron D3D11 texture import (Spike 2a, Windows)

**Files:**
- Modify: `spikes/vk-layer` (`cfg(windows)` client over named pipe `\\.\pipe\underwolf-spike-overlay`; import with `VK_KHR_external_memory_win32`, handle type `D3D11_TEXTURE`), `spikes/ipc` (`Msg::FrameWin { id, widget, x, y, width, height, nt_handle: u64 }` in place of `Frame` on Windows, plus a Windows `send`/`recv` without fds), `spikes/osr-bridge` (Windows: read `handle.ntHandle`, `DuplicateHandle` it into the game process opened with `PROCESS_DUP_HANDLE`, send the duplicated value)
- Create: `spikes/vk-layer/install.ps1` (registers the manifest under `HKCU\SOFTWARE\Khronos\Vulkan\ImplicitLayers`)

- [ ] **Step 1: Implement, then verify with vkcube and Deadlock** in default Vulkan mode in the Hideout. Same expectations as Task 5, Steps 2–4.
- [ ] **Step 2: Measure.** Run the bench (passthrough vs composite) → `PASS` expected. Record the results in `docs/spikes/02-shared-texture.md` under "Windows Vulkan".
- [ ] **Step 3: Commit.** `git commit -m "spikes: Windows Vulkan layer compositing D3D11 shared textures"`

### Task 15: Windows: DX11 present hook (Spike 2b)

**Files:**
- Create: `spikes/dx11-hook/` (crate `uw-spike-dx11`, cdylib DLL): it hooks `IDXGISwapChain::Present` and `ResizeBuffers` through vtable addresses taken from a dummy device and swapchain, using a detour crate (`retour` or `minhook`). It opens Electron textures with `ID3D11Device1::OpenSharedResource1`, copies each into a hook-owned texture, acks, and draws quads with its own state block saved and restored. It is draw-only: no other game function is hooked.
- Create: `spikes/dx11-hook/injector/` (binary `uw-spike-inject <pid> <dll>`: `OpenProcess` + `VirtualAllocEx` for the DLL path string + `CreateRemoteThread(LoadLibraryW)`). Writing the DLL path into the process is how the hook gets loaded, which is the same method the spec cites for Overwolf/Discord/OBS. Nothing else is written to the process.

- [ ] **Step 1: Verify with a DX11 sample app** (any Microsoft DirectX-Graphics-Samples D3D11 sample): widgets are composited, and resizing the window keeps working.
- [ ] **Step 2: Verify in Deadlock with `-dx11`** in the Hideout with the 3-widget load. Run the bench → `PASS` expected. Record the results in `docs/spikes/02-shared-texture.md` under "Windows DX11", and close out Spike 2 with a final verdict (gate: stop and report if not `GO`).
- [ ] **Step 3: Commit.** `git commit -m "spikes: DX11 present hook compositing Electron textures"`

### Task 16: Windows memory reader (Spike 3, Windows)

**Files:**
- Create: `spikes/dl-reader/src/win.rs` (`WinProcess` implementing `MemRead` via `OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION)` + `ReadProcessMemory`; modules via `EnumProcessModulesEx`/`GetModuleInformation`; `find_deadlock` via `Toolhelp32` snapshot + `is_target_cmdline`)
- Test: `reads_own_memory` and `read_unmapped_errors` run on Windows as well (`cfg` the Linux-only fixture test)

- [ ] **Step 1: Run the tests on Windows.** `cargo test -p uw-spike-dlreader` → PASS.
- [ ] **Step 2: Rerun Task 10, Step 4 (live schema test) and Task 12, Steps 1 and 3 on Windows.** Add a "Windows" column to the feature-by-source table and record any differences from Linux.
- [ ] **Step 3: Commit.** `git commit -m "spikes: Windows read-only Deadlock reader"`

### Task 17: Spike summary and owner review

**Files:** Create `docs/spikes/SUMMARY.md`

- [ ] **Step 1: Write the summary.** It has one line per spike and platform: verdict, chosen mechanism, and a link to the findings doc. Below that, a "Decisions needed from owner" list collected from every findings doc (at minimum: the reader access method; any compositor-dependent input behavior; whether AMD verification has been done; Proton-version fragility of the input mechanism). Below that, the proposed order of the milestone 1 plans.
- [ ] **Step 2: Commit.** `git commit -m "docs: spike summary"`
- [ ] **Step 3: Hand the summary to the owner for review** before any milestone plan is written.
