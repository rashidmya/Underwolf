# Underwolf Technical Spikes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Answer the three §8 spike questions (Linux input under Proton, shared-texture compositing, Deadlock live data via read-only memory reading) with working prototypes and measured go/no-go findings. The milestone plans will be written from these findings.

**Architecture:** One Cargo workspace with throwaway-quality but tested spike crates under `spikes/`: a Vulkan implicit layer (draws inside the game frame), an Electron offscreen-rendering host with a napi-rs bridge that passes shared textures to the layer over a Unix socket, an input harness, and an external read-only Deadlock memory reader. Each spike ends in a findings doc under `docs/spikes/`.

**Tech Stack:** Rust 1.96 (`ash`, `napi-rs` 3.x, `rustix` or `nix` for SCM_RIGHTS, `serde` + `postcard`, `windows` crate on Windows), Electron 44.x + TypeScript, Node 24.

**Spec:** `docs/superpowers/specs/2026-10-20-underwolf-design.md` (approved as-is, 2026-10-20)

## Global Constraints

- "Underwolf must never change the game's data or code or alter game behavior. Anything inside the game process is read/draw-only (drawing, input routing in edit mode, and read-only data collection)."
- "never change the game's data or code, never call game functions, never alter game behavior." Hooks on OS, graphics, input and timing functions are allowed (spec §6.3). (Owner decision 2026-10-21: Deadlock data is collected inside the game by the injected module, as Overwolf does; this replaces "never inject code for data collection". 2026-10-21: in-game architecture now copies Overwolf (spec §6.3 "In-game module architecture", §6.4). Owner: Linux only for now. **Amendment C replaces Tasks 9–13 with Tasks C1–C7, drops Tasks 14–16 and revises Task 17.**)
- "No redistribution of Overwolf code or apps." (The clean-room / no-reverse-engineering rule was lifted by the owner on 2026-10-21; see spec §3.)
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

Spike 2 (Linux) → Spike 1 (Linux) → Spike 3 (Tasks C1–C7, Amendment C; it reuses Spike 1's injected module) → Task 17 (revised in Amendment C). Tasks 9–16 below are superseded and kept only for history.

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

### Task 9 (superseded by Amendment C): Deadlock reader, process discovery and read-only access (Spike 3)

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

### Task 10 (superseded by Amendment C): Name-based offset resolution (Spike 3)

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

### Task 11 (superseded by Amendment C): Feature probes and the Overwolf-shaped emitter (Spike 3)

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

### Task 12 (superseded by Amendment C): Accuracy, latency and robustness runs (Spike 3)

**Files:**
- Create: `spikes/dl-reader/src/verify.rs`, plus a `uw-dlreader verify --jsonl out.jsonl --match-id <id>` subcommand. It fetches `https://api.deadlock-api.com/v1/matches/{match_id}/metadata` and compares final per-player kills/deaths/assists, hero and team, match outcome and match ID, printing a per-field `OK`/`MISMATCH`/`MISSING` table.
- Modify: `docs/spikes/03-deadlock-memory.md`

- [ ] **Step 1: Run accuracy checks.** Record `watch` through at least 3 real matches (read-only reader only, no overlay layer installed) and 1 bot match. Run `verify` on the real matches. Expected: no `MISMATCH`. Check the bot match by hand against the end-of-match scoreboard.
- [ ] **Step 2: Check latency.** During the bot match, screen-record alongside `watch`, and for 5 kills compare the kill-feed time with the `kill` event's `t_ms`. Expected: ≤ 1 s each.
- [ ] **Step 3: Check fail-safe (Review Focus 5).** Run `UW_DLREADER_CORRUPT=<Class>.<field> uw-dlreader watch`, which adds 8 to that offset after resolution, once each for the kills field and the team field. Expected: the emitter goes silent and the status table shows `silenced`, with no wrong values in the JSONL before that.
- [ ] **Step 4: Check robustness across updates.** After the next Deadlock patch, rerun `watch` with **no code changes**. Record which features survived, which patterns broke, and the schema-dump diff (`diff` of the two `schema-<build>.json` files). If no patch lands within the spike window, use Steam's `download_depot` to get the previous build's `client.dll` and run the pattern scans against it statically, and record that the schema side wasn't tested.
- [ ] **Step 5: Commit.** `git commit -m "spikes: Deadlock reader accuracy and robustness results"`

### Task 13 (superseded by Amendment C): Spike 3 (Linux) findings

**Files:** Modify `docs/spikes/03-deadlock-memory.md` (from the template)

- [ ] **Step 1: Write the findings, including the feature-by-source table** that spec §8.3 asks for. Columns: Feature/key | Source (memory / console log / deadlock-api.com / unavailable) | Resolution (schema name / pattern) | Accuracy result | Survived patch | Notes. Add one row per §6.4 feature and event. For each feature that memory doesn't provide, name the supplementary source the spec allows (console log, local state files, deadlock-api.com for `match_history` and post-match), or mark it `unavailable`. Include the access-method decision for the owner (Task 9, Step 5).
- [ ] **Step 2: Gate.** If live features (`phase`, `roster`, `kill`/`death`/`assist`, `match_clock`) aren't readable, stop and report to the owner.
- [ ] **Step 3: Commit.** `git commit -m "docs: spike 3 Linux findings"`

### Task 14 (dropped, Amendment C): Windows: Vulkan layer and Electron D3D11 texture import (Spike 2a, Windows)

**Files:**
- Modify: `spikes/vk-layer` (`cfg(windows)` client over named pipe `\\.\pipe\underwolf-spike-overlay`; import with `VK_KHR_external_memory_win32`, handle type `D3D11_TEXTURE`), `spikes/ipc` (`Msg::FrameWin { id, widget, x, y, width, height, nt_handle: u64 }` in place of `Frame` on Windows, plus a Windows `send`/`recv` without fds), `spikes/osr-bridge` (Windows: read `handle.ntHandle`, `DuplicateHandle` it into the game process opened with `PROCESS_DUP_HANDLE`, send the duplicated value)
- Create: `spikes/vk-layer/install.ps1` (registers the manifest under `HKCU\SOFTWARE\Khronos\Vulkan\ImplicitLayers`)

- [ ] **Step 1: Implement, then verify with vkcube and Deadlock** in default Vulkan mode in the Hideout. Same expectations as Task 5, Steps 2–4.
- [ ] **Step 2: Measure.** Run the bench (passthrough vs composite) → `PASS` expected. Record the results in `docs/spikes/02-shared-texture.md` under "Windows Vulkan".
- [ ] **Step 3: Commit.** `git commit -m "spikes: Windows Vulkan layer compositing D3D11 shared textures"`

### Task 15 (dropped, Amendment C): Windows: DX11 present hook (Spike 2b)

**Files:**
- Create: `spikes/dx11-hook/` (crate `uw-spike-dx11`, cdylib DLL): it hooks `IDXGISwapChain::Present` and `ResizeBuffers` through vtable addresses taken from a dummy device and swapchain, using a detour crate (`retour` or `minhook`). It opens Electron textures with `ID3D11Device1::OpenSharedResource1`, copies each into a hook-owned texture, acks, and draws quads with its own state block saved and restored. It is draw-only: no other game function is hooked.
- Create: `spikes/dx11-hook/injector/` (binary `uw-spike-inject <pid> <dll>`: `OpenProcess` + `VirtualAllocEx` for the DLL path string + `CreateRemoteThread(LoadLibraryW)`). Writing the DLL path into the process is how the hook gets loaded, which is the same method the spec cites for Overwolf/Discord/OBS. Nothing else is written to the process.

- [ ] **Step 1: Verify with a DX11 sample app** (any Microsoft DirectX-Graphics-Samples D3D11 sample): widgets are composited, and resizing the window keeps working.
- [ ] **Step 2: Verify in Deadlock with `-dx11`** in the Hideout with the 3-widget load. Run the bench → `PASS` expected. Record the results in `docs/spikes/02-shared-texture.md` under "Windows DX11", and close out Spike 2 with a final verdict (gate: stop and report if not `GO`).
- [ ] **Step 3: Commit.** `git commit -m "spikes: DX11 present hook compositing Electron textures"`

### Task 16 (dropped, Amendment C): Windows memory reader (Spike 3, Windows)

**Files:**
- Create: `spikes/dl-reader/src/win.rs` (`WinProcess` implementing `MemRead` via `OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION)` + `ReadProcessMemory`; modules via `EnumProcessModulesEx`/`GetModuleInformation`; `find_deadlock` via `Toolhelp32` snapshot + `is_target_cmdline`)
- Test: `reads_own_memory` and `read_unmapped_errors` run on Windows as well (`cfg` the Linux-only fixture test)

- [ ] **Step 1: Run the tests on Windows.** `cargo test -p uw-spike-dlreader` → PASS.
- [ ] **Step 2: Rerun Task 10, Step 4 (live schema test) and Task 12, Steps 1 and 3 on Windows.** Add a "Windows" column to the feature-by-source table and record any differences from Linux.
- [ ] **Step 3: Commit.** `git commit -m "spikes: Windows read-only Deadlock reader"`

### Task 17 (superseded by the revised Task 17 in Amendment C): Spike summary and owner review

**Files:** Create `docs/spikes/SUMMARY.md`

- [ ] **Step 1: Write the summary.** It has one line per spike and platform: verdict, chosen mechanism, and a link to the findings doc. Below that, a "Decisions needed from owner" list collected from every findings doc (at minimum: the reader access method; any compositor-dependent input behavior; whether AMD verification has been done; Proton-version fragility of the input mechanism). Below that, the proposed order of the milestone 1 plans.
- [ ] **Step 2: Commit.** `git commit -m "docs: spike summary"`
- [ ] **Step 3: Hand the summary to the owner for review** before any milestone plan is written.

---

## Amendment A (2026-10-20): CPU copy for widget frames (owner decision)

**Why:** On NVIDIA 615, Electron 44/45 shared-texture frames are empty (evidence in
`docs/spikes/02-shared-texture.md`). The owner chose to **allow a CPU copy for widget frames**.
This replaces the GPU shared-texture leg in Tasks 4–5. The dmabuf code and its tests stay (they
are needed for AMD/Windows later), but the host no longer uses them.

**Design:**
- **Host:** Electron CPU offscreen rendering (`webPreferences.offscreen: true`) with **default
  Chromium switches**, so the GPU sandbox is back on. The `paint` event's `NativeImage` gives a
  BGRA, premultiplied bitmap.
- **Shared memory:** each widget has 3 shared-memory slots (memfd, size `stride × height` rounded
  up to 4096). The host copies the bitmap into a free slot: this is the only CPU copy. A slot is
  busy from send until ack or timeout. If no slot is free, the frame is dropped without sending.
- **Layer:** imports each slot's mapping as a `VkBuffer` via `VK_EXT_external_memory_host`
  (supported on this GPU, alignment 0x1000), so the GPU reads straight from the shared pages.
  Each present, for every widget with a new frame, it records `vkCmdCopyBufferToImage` slot →
  layer-owned widget image, then draws a textured quad. It acks a frame once that submission's
  fence has signalled (polled without blocking at each present).

**ipc additions (Task 4):**
```rust
Msg::Slot { widget: u32, slot: u32, width: u32, height: u32, stride: u32, drm_fourcc: u32 } // host → layer, 1 memfd attached; sent once per slot per peer (and again if the size changes)
Msg::FrameShm { id: u64, widget: u32, slot: u32, x: i32, y: i32 }                       // host → layer, no fds
```

**Bridge (Task 4):** `Server::send_bitmap(&self, b: &Bitmap, timeout) -> SendResult` with
`Bitmap { widget, x, y, width, height, stride, pixel_format, data: &[u8] }`. JS:
`sendBitmap({ widget, x, y, width, height, stride, pixelFormat, data: Buffer }): Promise<'acked'|'dropped'|'timeout'>`.
The copy into the slot happens synchronously in the call; only the ack wait is async.

**Revised Task 4, Steps 6a–6c:**
- 6a. Failing bridge tests: `send_bitmap_registers_slot_once_per_peer` (two sends → exactly one
  `Slot` for that slot index, then `FrameShm`s); `send_bitmap_memfd_contains_pixels` (the peer
  mmaps the received memfd and reads back the exact bytes sent); `send_bitmap_drops_when_all_slots_busy`
  (3 unacked sends with long timeouts in flight → the 4th returns `Dropped` immediately);
  `send_bitmap_drops_without_peer`. Then implement, then green.
- 6b. Host switches to CPU offscreen rendering with default Chromium switches. The consumer handles
  `Slot` + `FrameShm`, mmaps the slot, prints the center pixel as `RRGGBBAA` and acks.
- 6c. End-to-end check (same command as Step 6) → consumer prints `ff0080ff`.

**Revised Task 5:** as written, except the import is the memfd/host-pointer path above, and
`QuadSource::Image(vk::ImageView)` samples the layer-owned widget image. The benchmark runs the
3-widget load in two scenarios:
- CPU-bound: the default Hideout run.
- GPU-bound: raise graphics settings and render resolution until FPS drops and varies between
  runs.

Both must `PASS`. Also record the host's CPU time per widget frame (copy + readback) from a
`performance.now()` timer around the slot copy.

---

## Amendment B (2026-10-20): Spike 1 session matrix is Wayland-only (owner decision)

Current GNOME (50+) and KDE Plasma (6.8+, the test machine's version) no longer ship X11
sessions. Task 7's matrix is now **KDE Wayland** and **GNOME Wayland**; the game runs through
XWayland in both. The owner installs GNOME on the test machine. The spec's §7a and §8 have been
updated to match.

---

## Amendment C (2026-10-21): in-game Spike 3, Linux only (owner decisions)

**Why:** The spec now says Deadlock data is read inside the game, the Overwolf way (§6.3 "In-game
module architecture", §6.4 "Loading and timing", "Data channel"). Tasks 9–13 were written for an
external `/proc` reader, so they are replaced by Tasks C1–C7 below. Tasks 14–16 (Windows) are
**dropped**, because Windows is unscheduled. Task 17 is revised at the end of this amendment.

**Owner decisions (2026-10-21):**
- Hook engine: **MinHook** (`minhook` crate 0.9; it is Overwolf's default). This is the one inline
  engine for every in-game hook. Spike 3 uses it only for the QPC hook. C3's IAT input hooks move
  to MinHook in milestone 1. Record this in spec §6.3 (Task C7).
- Per-frame `present`: the in-game module calls the plugin's `present` about every 16 ms from its
  own thread. This is a Linux difference: the Windows-side module has no present hook, because
  drawing happens in the native Vulkan layer.
- Real matches: **collector only**. The in-game module is injected with input hooks off, with no
  Vulkan layer and no Electron host. Hideout and bot matches run everything.
- Features underlock didn't verify (`game_mode`, `game_paused`, `team_score`, `match_outcome`,
  `tower_destroyed`, `incoming_damage`) get a **2-day timebox**. Anything not found goes into the
  table as "other source" or "unavailable". Only the verified features gate GO.

**Inputs:** `~/Dev/underwolf-research/docs` (README and "Behavior-level summary" sections only);
`~/Dev/underlock/src/games/native/{source2,deadlock-roster,deadlock-clock}.js` for field lists
and read rules (phase, match_id, roster, items and clock were checked live there).

**Evidence before planning (2026-10-21, offline, read-only):** Deadlock updated to ClientVersion
6801 (`client.dll` SHA-256 `a7d0d9e8…589e`). underlock's `inspectDeadlockModule` still resolves
every byte pattern and schema field on it (field offsets unchanged, RVAs moved). The clock is
lost because it was pinned to the old file's hash. So the clock's roots must become byte patterns
(Task C3).

**Architecture (Spike 3):**
```
deadlock.exe (Wine)                                   Wine session          Linux
 ├ uw_spike_input.dll  (in-game module, C3) ──TCP 48610──────────────────▶ uw-gep (client)
 │    loads plugin on LoadPlugin, calls present every 16 ms                  │ injects module,
 └ uw_spike_dlcollector.dll  (plugin)                                       │ starts helper,
      MinHook on QPC, acts only on the main-loop caller                     │ provider → JSONL
      tasks → info transaction / event ring                                 │
      writes named shared memory  ──▶ uw-gep-helper.exe ──TCP 48611────────▶┘
```
- New crates: `spikes/gep-channel` (data channel: pure layout logic plus `cfg(windows)` named
  objects), `spikes/dl-collector` (plugin DLL plus portable core), `spikes/gep-helper` (Wine-side
  relay), `spikes/gep-host` (Linux client `uw-gep`). Changed: `spikes/input` (command reader,
  plugin loader), `spikes/ipc` (new `Msg` variants), `spikes/inject` (reusable session lookup),
  `spikes/osr-host` (sends `ModuleConfig`).
- Every PE crate builds with `--target x86_64-pc-windows-gnu`. Portable logic is unit-tested on
  Linux; `cfg(windows)` FFI is checked by the owner's in-game runs.

**Constraints added by this amendment:**
- Reads inside the game go through `MemRead` only. The in-game implementation is
  `ReadProcessMemory(GetCurrentProcess(), …)` and requires the full byte count, so an unmapped
  address returns an error and never faults. No raw pointer dereference of game memory anywhere.
- Pointers are plausible only in `0x1000..=0x7fff_ffff_ffff`. Counts are bounded before reading
  (teams ≤ 32, controllers per team ≤ 64, upgrades ≤ 64, name ≤ 128 bytes).
- The collector never calls game functions and never writes game memory. Its only hook is
  `QueryPerformanceCounter` (OS timing API).
- No offsets are pinned to a file hash. Roots come from masked byte patterns; fields come from
  class and field names in the game's schema data.

**Review Focus (Spike 3):**
1. **A game update moves code or layouts.** Required roots missing → status `error` with a reason
   and nothing published. Per-task roots missing → that task is disabled and reported, the rest
   keep running. Tests: `unresolved_required_root_publishes_only_error_status` (C3),
   `missing_clock_root_disables_only_clock` (C3).
2. **Torn reads while the game changes state** (match load, player joins, item bought). A pass
   that sees data change mid-read publishes nothing for that task. Tests:
   `roster_change_during_read_is_discarded`, `items_reallocation_is_discarded` (C3).
3. **The helper or `uw-gep` restarts while the game runs.** The collector keeps writing; a new
   helper reconnects and gets a full snapshot. Test: `consumer_attaching_late_reads_full_snapshot`
   (C1), plus Task C5 Step 5 and Task C6 Step 6.
4. **The QPC hook runs on every game thread, thousands of times per second.** The non-matching
   path must do nothing but a compare and a tail call; a pass must never re-enter itself. Tests:
   `non_matching_caller_does_not_run_pass`, `pass_does_not_reenter` (C4), plus the C6 bench.
5. **A slow or crashing task.** It is disabled and reported, and the game keeps its frame rate.
   Tests: `slow_task_disabled_after_ten_overruns`, `panicking_task_disabled_immediately` (C3).

### Task C1: Data channel, Overwolf-style shared memory (Spike 3)

**Files:**
- Create: `spikes/gep-channel/` (crate `uw-spike-gep-channel`, lib): `src/lib.rs`, `src/schema.rs`,
  `src/info.rs`, `src/events.rs`, `src/throttle.rs`, `src/win.rs` (`cfg(windows)` named objects)
- Test: `spikes/gep-channel/tests/{schema.rs,info.rs,events.rs,throttle.rs}`

**Interfaces:**
- Produces (portable, over plain byte slices so Linux tests cover the layout):
  ```rust
  pub const FORMAT_VERSION: u32 = 1;
  pub struct Schema { pub categories: Vec<(String, Vec<(String, u32 /*max len*/)>)> }
  impl Schema { pub fn serialize(&self) -> Vec<u8>; pub fn parse(b: &[u8]) -> Result<Schema, ChannelError>;
                pub fn key_index(&self, category: &str, key: &str) -> Option<usize>; }
  pub struct InfoWriter<'a> { /* region: &'a mut [u8] */ }
  impl InfoWriter<'_> { pub fn init(region: &mut [u8], schema: &Schema) -> Result<InfoWriter<'_>, ChannelError>;
      pub fn begin(&mut self) -> Txn<'_>; }
  impl Txn<'_> { pub fn set(&mut self, category: &str, key: &str, value: Option<&[u8]>, ts: u64) -> Result<(), ChannelError>;
      pub fn commit(self) -> bool /* true if anything changed: signal once */; }
  pub struct InfoReader { /* schema + last seen per-key ts */ }
  impl InfoReader { pub fn attach(region: &[u8]) -> Result<InfoReader, ChannelError>;
      pub fn changed(&mut self, region: &[u8]) -> Vec<InfoChange>; pub fn snapshot(&self, region: &[u8]) -> Vec<InfoChange>; }
  pub struct InfoChange { pub category: String, pub key: String, pub value: Option<Vec<u8>>, pub ts: u64 }
  pub const RING_SLOTS: usize = 30; pub const NAME_MAX: usize = 255; pub const DATA_MAX: usize = 63 * 1024;
  pub fn ring_init(region: &mut [u8]); pub fn ring_push(region: &mut [u8], name: &str, data: &[u8], ts: u64) -> Result<(), ChannelError>;
  pub struct RingReader { next_seq: u64 }
  impl RingReader { pub fn drain(&mut self, region: &[u8]) -> (Vec<EventRecord>, u64 /*lost*/); }
  pub struct EventRecord { pub name: String, pub data: Vec<u8>, pub ts: u64 }
  pub struct Throttle { /* window_ticks, max, sent timestamps */ }
  impl Throttle { pub fn new(window_ticks: u64, max: usize) -> Throttle; pub fn admit(&mut self, now: u64) -> bool; }
  pub enum ChannelError { TooLong, UnknownKey, BadMagic, BadVersion, Truncated }
  ```
- Layout (our own, from the behavior summary): info region = header `{magic "UWGI", version u32,
  schema_len u32, key_count u32, slot_area_off u32, commit_seq u64}`, then the serialized schema,
  then `key_count × u64` timestamps, then per-key slots `{len u32, bytes[max]}` (8-aligned).
  `len = u32::MAX` means "cleared" (null), distinct from an empty value. Event region = header
  `{magic "UWGE", version, write_seq u64}` then 30 records `{seq u64, ts u64, name_len u16,
  name[255], data_len u32, data[63 KiB]}`; record `seq % 30` is overwritten.
- `Txn::set` with an unchanged value does not touch the slot or its timestamp. Values longer than
  the key's max are rejected (`TooLong`), never truncated. Unknown keys are rejected.
- Throttle: at most 30 events per window; window = `QueryPerformanceFrequency / 5` ticks
  (200 ms). The collector keeps throttled events in a FIFO of at most 256 and retries them each
  pass (C3).
- `cfg(windows)` `win.rs`: `Producer::create(channel: u32, schema: &Schema) -> io::Result<Producer>`
  and `Consumer::open(channel: u32) -> io::Result<Consumer>`. Object names (all `Local\`):
  `uw-gep-<channel>-{hello,info,events}` (mappings), `-{info,events}-mutex` (mutexes),
  `-{connect,disconnect}` (manual-reset events), `-{info,events}-signal` (auto-reset events).
  The hello mapping holds `{version u32, pid u32}`. The producer sets `connect` after both
  regions are initialised and sets `disconnect` (and resets `connect`) on drop.
  `Consumer::wait(&self, timeout_ms) -> Wake { Info, Events, Disconnect, Timeout }`.

- [ ] **Step 1: Write the failing tests.**
  - `schema_roundtrip`: a schema with `gep_internal{version:64,plugin_status:32}` and
    `match_info{roster_0:2048}` survives `serialize`/`parse`, and `key_index("match_info","roster_0") == Some(2)`.
  - `txn_commits_once_and_skips_unchanged`: set `phase="GameInProgress"` → `commit() == true`;
    the same value again → `commit() == false` and the key's timestamp is unchanged.
  - `null_is_distinct_from_empty`: `set(.., Some(b""))` and `set(.., None)` read back as
    `Some(vec![])` and `None`.
  - `too_long_value_is_rejected`: 65 bytes into a max-64 key → `Err(TooLong)` and the old value stays.
  - `consumer_attaching_late_reads_full_snapshot` (Review Focus 3): write 3 keys, then
    `InfoReader::attach` → `snapshot()` returns all 3; `changed()` right after returns nothing.
  - `ring_overwrites_oldest_and_reports_loss`: push 35 events without draining → `drain` returns
    the last 30 in order and `lost == 5`.
  - `throttle_admits_30_per_window`: 30 `admit(t)` calls true, the 31st false; after
    `t + window` admits again.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-gep-channel` → compile errors.
- [ ] **Step 3: Implement** the portable modules, then `win.rs` (`windows` 0.58 features:
  `Win32_System_Memory`, `Win32_System_Threading`, `Win32_Foundation`, `Win32_Security`).
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-gep-channel` → PASS, and
  `cargo check --target x86_64-pc-windows-gnu -p uw-spike-gep-channel` → no errors.
- [ ] **Step 5: Commit.** `git commit -m "spikes: Overwolf-style GEP data channel (info store, event ring, throttle)"`

### Task C2: Collector core: safe reads, patterns, schema fields by name (Spike 3)

**Files:**
- Create: `spikes/dl-collector/` (crate `uw-spike-dlcollector`, `crate-type = ["lib", "cdylib"]`,
  lib name `uw_spike_dlcollector`): `src/mem.rs`, `src/module.rs`, `src/pattern.rs`,
  `src/schema.rs`, `src/roots.rs`
- Test: `spikes/dl-collector/tests/{pattern.rs,schema.rs,roots.rs,installed_build.rs}`
  (`installed_build.rs` is `#[ignore]`; it reads the installed `client.dll` from disk)

**Interfaces:**
- Produces:
  ```rust
  pub trait MemRead { fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ReadError>; }
  // helpers on dyn MemRead: read_u8/u32/i32/u64/f32, read_ptr (plausible range or Err),
  // read_cstr(addr, max) -> Result<String, ReadError> (NUL within max, valid UTF-8)
  pub struct FakeMem { /* Vec<(base, Vec<u8>)> */ }   // tests
  #[cfg(windows)] pub struct SelfProcess;              // ReadProcessMemory(GetCurrentProcess())
  pub struct Module { pub name: String, pub base: u64, pub image_size: u32, pub sections: Vec<Section> }
  pub struct Section { pub rva: u32, pub size: u32, pub executable: bool }
  impl Module { pub fn from_headers(m: &dyn MemRead, name: &str, base: u64) -> Result<Module, ReadError>; }
  pub fn file_image(bytes: &[u8]) -> Result<(FakeMem, u64 /*preferred ImageBase*/), ReadError>; // maps sections at their RVAs
  pub struct Pattern(Vec<Option<u8>>);
  impl Pattern { pub fn parse(s: &str) -> Result<Pattern, PatternError>; }  // "48 8b ?? c3"
  pub fn scan(m: &dyn MemRead, module: &Module, p: &Pattern, limit: usize) -> Result<Vec<u64>, ReadError>; // executable sections only
  pub fn rip_target(m: &dyn MemRead, site: u64, disp_at: u32, next: u32) -> Result<u64, ReadError>;
  pub struct SchemaFields { /* (class, field) -> u32 */ }
  impl SchemaFields { pub fn load(m: &dyn MemRead, client: &Module) -> Result<SchemaFields, RootError>;
      pub fn get(&self, class: &str, field: &str) -> Option<u32>; }
  pub struct Roots { pub game_rules_ptr: u64, pub teams: u64, pub entity_list_ptr: u64,
                     pub entity_stride: u32, pub serial_offset: u32 }
  pub fn resolve_roots(m: &dyn MemRead, client: &Module) -> Result<Roots, RootError>;
  pub enum RootError { NotFound(&'static str), Ambiguous(&'static str), Read(ReadError) }
  ```
- Patterns and checks are the ones in `~/Dev/underlock/src/games/native/source2.js`, copied as
  strings into `roots.rs` with a comment naming that file and build 6801:
  - game rules: `RULES`, accepted only if the string at `rip_target(site, 0x21, 0x25)` equals
    `"%s:  CGameRules::CGameRules constructed\n"`; result `rip_target(site, 3, 7)`; exactly one.
  - schema class table: `CLASSES` → `rip_target(site, 3, 7)` is a NUL-terminated array of class
    record pointers (≤ 20000); class name pointer at record `+8`. Field table layouts tried:
    `{count u16 at +0x24, table ptr at +0x30}` then `{+0x1c, +0x28}`; entries are 32 bytes with
    name pointer at `+0` and offset `u32` at `+16`. A class name in more than one record, or a
    field found twice, is `Ambiguous`.
  - teams: `TEAMS`, root = `rip_target(site, 6, 10)`, accepted only if `rip_target(site, 15, 19) == root + 8`.
  - entity list: `ENTITIES` (limit 4096), stride = byte at `site + 48`, serial offset = byte at
    `site + 56`, valid when `24 ≤ stride < 128` and `8 ≤ serial + 4 ≤ stride`; all hits must agree.
- In the game the module bases come from `GetModuleHandleW`; offline, `file_image` maps sections
  so the same `resolve_roots` and `SchemaFields::load` run against the file (pointers in the file
  are relative to the preferred ImageBase, which `file_image` uses as `Module::base`).

- [ ] **Step 1: Write the failing tests.**
  - `pattern_parse_and_scan`: `Pattern::parse("48 8B 05 ?? ?? ?? ?? C3")` finds offset 1 in
    `[0x90,0x48,0x8B,0x05,1,2,3,4,0xC3]` placed in an executable section; `parse("4G")` → `Err`;
    the same bytes in a non-executable section are not found.
  - `rip_target_decodes_signed_displacement`: disp `+0x10` at site `0x1000`, `(3,7)` → `0x1017`;
    disp `-0x10` → `0xFF7`.
  - `read_unmapped_is_error_not_fault`: `FakeMem` read at `0x8` → `Err`; `read_ptr` of value
    `0x10` → `Err` (implausible).
  - `schema_resolves_class_qualified_field`: a synthetic class table in `FakeMem` with
    `C_CitadelGameRules{m_eGameState:0x74, m_unMatchID:0xa060}` → `get` returns those;
    `get("C_CitadelGameRules","m_nope") == None`.
  - `duplicate_class_is_ambiguous`: two records named `C_Team` → looking up any `C_Team` field
    makes `SchemaFields::load` keep it out (`get` → `None`), and the reason is recorded.
  - `rules_requires_marker_string`: a `RULES` hit whose string differs → `NotFound("game rules")`.
  - `installed_build` (ignored): on the installed `client.dll`, `resolve_roots` succeeds and
    `SchemaFields` gives `m_eGameState=0x74`, `m_unMatchID=0xa060`, `C_Team.m_aPlayerControllers=0x5f0`,
    `CBasePlayerController.m_bIsLocalPlayerController=0x790`, `PlayerDataGlobal_t.m_vecUpgrades=0xa0`
    (values from the 6801 check above).
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-dlcollector` → compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, verify pass.** `cargo test -p uw-spike-dlcollector`, then
  `cargo test -p uw-spike-dlcollector -- --ignored installed_build` → PASS.
- [ ] **Step 5: Add the offline schema dump.** Example `cargo run -p uw-spike-dlcollector --example
  dump_schema -- <client.dll> > spikes/dl-collector/dumps/schema-<ClientVersion>.json`
  (class → field → offset, sorted). Commit the 6801 dump; Task C6 uses it for the timeboxed features.
- [ ] **Step 6: Commit.** `git commit -m "spikes: collector core (safe self-reads, patterns, schema fields by name)"`

### Task C3: Collection tasks, task runner, fail-safe (Spike 3)

**Files:**
- Create: `spikes/dl-collector/src/{runner.rs,keys.rs,collector.rs}`,
  `spikes/dl-collector/src/tasks/{mod.rs,phase.rs,match_id.rs,roster.rs,clock.rs}`
- Modify: `spikes/dl-collector/src/roots.rs` (clock roots)
- Test: `spikes/dl-collector/tests/{runner.rs,tasks.rs,collector.rs}`

**Interfaces:**
- Consumes: `MemRead`, `FakeMem`, `Module`, `SchemaFields`, `Roots` (C2); `Schema`, `Txn`,
  `ring_push`, `Throttle` (C1).
- Produces:
  ```rust
  pub fn deadlock_schema() -> Schema;   // keys.rs, fixed at start-up
  pub trait Task { fn name(&self) -> &'static str;
                   fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError>; }
  pub enum TaskError { Torn, Read(ReadError), Invalid(String) }
  pub struct Pass { /* staged info sets + staged events for this pass */ }
  impl Pass { pub fn set(&mut self, cat: &str, key: &str, v: Option<String>); pub fn event(&mut self, name: &str, data: String); }
  pub struct Runner { /* tasks, per-task {overruns, disabled: Option<String>} */ }
  impl Runner { pub fn new(tasks: Vec<Box<dyn Task>>) -> Runner;
      pub fn pass(&mut self, m: &dyn MemRead, clock: &dyn Fn() -> u64, freq: u64) -> Pass;
      pub fn disabled(&self) -> Vec<(&'static str, String)>; }
  pub enum Status { Shutdown, Initializing, Initialized, Scanning, Error(String) }
  pub struct Collector { /* status, runner, info writer, ring, throttle, retry FIFO */ }
  impl Collector { pub fn start(m: &dyn MemRead, client: &Module, out: Box<dyn ChannelOut>, args: &str) -> Collector;
                   pub fn tick(&mut self, m: &dyn MemRead); pub fn status(&self) -> &Status; }
  pub trait ChannelOut { fn commit(&mut self, sets: &[(String, String, Option<Vec<u8>>)], ts: u64);
                         fn push_event(&mut self, name: &str, data: &[u8], ts: u64) -> bool /*false = throttled*/; }
  ```
- Fixed schema (`keys.rs`, max lengths in bytes): `gep_internal{version_info:64, plugin_status:32,
  plugin_error:256, disabled_tasks:1024, pass_stats:128}` (`pass_stats` = JSON
  `{"median_ms","max_ms","passes"}`, published every 10 s); `game_info{phase:64, game_mode:256}`;
  `match_info{match_id:32, match_clock:16, game_paused:8, team_score:64, match_outcome:64,
  roster_0..roster_11:1024, items_0..items_11:1024}`. Keys and names follow spec §6.4; exact
  Overwolf value formats are deferred to the provider plan.
- Runner rules (spec §6.4): each task is timed with the clock passed in. A run over 20 ms counts
  as an overrun; the 11th overrun disables the task (`"slow"`). A panic (caught with
  `catch_unwind`) disables it at once (`"crashed"`). `Invalid` disables it at once
  (`"invalid: <reason>"`, a layout mismatch). `Torn` and `Read` drop that task's sets for this
  pass only. All tasks of one pass go into one `ChannelOut::commit` (one signal). The disabled
  list is published as `gep_internal.disabled_tasks` (JSON `[{task, reason}]`).
- Fail-safe (Review Focus 1): `Collector::start` resolves `Roots` and the required fields
  (`C_CitadelGameRules.m_eGameState`, `.m_unMatchID`). If any is missing it sets
  `Status::Error(reason)`, publishes only `plugin_status="error"` and `plugin_error`, and `tick`
  does nothing after that. Per-task roots/fields missing → that task is not created and is listed
  in `disabled_tasks` with `"unresolved: <what>"`.
- Tasks (fields and read rules from underlock's `deadlock-roster.js` / `deadlock-clock.js`):
  - **phase**: `m_eGameState` (i32) → names `Invalid, Init, WaitingForPlayersToJoin, HeroSelection,
    MatchIntro, WaitForMapToLoad, PreGameWait, GameInProgress, PostGame, PostGame_PlayOfTheGame,
    Abandoned, End` (index 0–11); out of range → `Invalid`. Game rules pointer null → clear
    phase, match_id, clock, all roster/items keys.
  - **match_id**: `m_unMatchID` as **u32**; 0 → not set.
  - **roster** (also publishes items): the team vector at `Roots::teams` (`count i32 at +0`, data
    ptr at `+8`), each team's `C_Team.m_aPlayerControllers` vector of u32 handles, handle →
    entry `page[(h>>9)&0x3f] + (h & 0x1ff) * stride`, accepted only if the u32 at
    `entry + serial_offset == h`; controller = pointer at entry `+0`. Per player:
    `CBasePlayerController.m_iszPlayerName` (≤ 128, UTF-8), `.m_steamID` (u64 → decimal string),
    `.m_bIsLocalPlayerController` (0/1 else `Invalid`), `C_BaseEntity.m_iTeamNum` (u8, read on
    the team object), `CCitadelPlayerController.m_PlayerDataGlobal` + `PlayerDataGlobal_t.{m_nHeroID,
    m_iPlayerKills, m_iDeaths, m_iPlayerAssists}` (u32), `m_vecUpgrades` (vector of u32 item ids,
    ≤ 64). Hero 0 → not a roster entry. Every vector header, handle list, entity page and entry is
    re-read after use; any difference → `Torn`. More than one local player, a duplicate
    name+steam identity, or more than 12 entries → `Invalid`. Slots `0..11` are stable per
    identity; a removed identity clears its `roster_N`/`items_N`. Values: `roster_N =
    {"player_name","steam_id","hero_id","team_id","is_local","kills","deaths","assists"}`,
    `items_N = {"player_name","steam_id","items":[{"id"}]}`.
  - **clock** (only while phase = `GameInProgress`): the arithmetic in underlock's
    `readDeadlockClock`, with its five roots found by byte patterns instead of a file hash:
    globals pointer (time f32 at `+0x30`, tick interval f32 at `+0x54`), the expected game-time
    adjust function (checked against game rules vtable `+0x220`), and the data pointers of the
    clock-offset, pregame-offset and pregame-enabled ConVars (value at `+0x58`). Game-rules fields
    by schema name where the 6801 dump has them (paused ticks, pause-start tick, paused flag,
    game start time); otherwise the task is unresolved. Publishes `match_clock` as `"MM:SS"` and
    event `clock` when the string changes. Paused flag false→true → event `game_paused`, and
    `game_paused` info.
- Start args (string from `LoadPlugin`, C4; space-separated): `corrupt=<Class>.<field>` adds 8 to
  that resolved offset (fail-safe run, C6 Step 5). `selftest=1` skips root resolution and
  publishes `plugin_status="selftest"` plus one event `selftest` (C5 Step 5). `discover=<s>` (C4).

- [ ] **Step 1: Find the clock roots on build 6801.** Starting from underlock's notes (the
  `%d:%02d` formatter and the arithmetic in `deadlock-native.md` "Current-client live clock"),
  locate the five roots in the installed `client.dll` (REA/Ghidra allowed on the game binary), and
  write each as a masked pattern + `rip_target` decode in `roots.rs` with a comment naming the build.
  Add assertions for them to `installed_build` (each resolves to exactly one address).
- [ ] **Step 2: Write the failing tests** (all on `FakeMem` images built in the test):
  - `phase_and_match_id_publish_names`: state 7, match 1234 → `phase="GameInProgress"`, `match_id="1234"`.
  - `null_game_rules_clears_match_keys`: after a valid pass, rules pointer 0 → phase, match_id,
    clock and `roster_0` become `None`.
  - `roster_reads_twelve_players_and_one_local`: 2 teams × 6 → 12 `roster_N` with exactly one
    `"is_local":true`; steam id `76561198000000001` stays a string.
  - `stale_handle_is_skipped`: an entry whose serial ≠ handle → that player is absent, no error.
  - `roster_change_during_read_is_discarded` (Review Focus 2): `FakeMem` that changes a team's
    handle vector between first read and re-read → `Torn`, no roster key set this pass.
  - `items_reallocation_is_discarded`: upgrades pointer changes between reads → `Torn`.
  - `two_local_players_is_invalid`: → `Invalid`, task disabled with reason `invalid: …`.
  - `removed_player_clears_slot`: player leaves → their `roster_N` and `items_N` set to `None`,
    the others keep their slots.
  - `clock_matches_underlock_cases`: port the expected values from
    `~/Dev/underlock/tests/deadlock-clock.test.js` (pause freeze, paused-tick subtraction,
    pregame offset in mode 1).
  - `slow_task_disabled_after_ten_overruns` (Review Focus 5): a fake clock making one task take
    25 ms → enabled after 10 passes, disabled after the 11th with `"slow"`; other tasks still run.
  - `panicking_task_disabled_immediately`: → `"crashed"` after one pass; others still publish.
  - `one_commit_per_pass`: 3 tasks each setting a key → exactly one `ChannelOut::commit` call.
  - `unresolved_required_root_publishes_only_error_status` (Review Focus 1): no `RULES` hit →
    `Status::Error`, the only keys ever committed are `plugin_status="error"` and `plugin_error`,
    and later `tick`s commit nothing.
  - `missing_clock_root_disables_only_clock`: → `disabled_tasks` contains
    `{"task":"clock","reason":"unresolved: …"}`, phase and roster still publish.
  - `throttled_events_are_retried_in_order`: `push_event` returns false for 2 events → they are
    pushed first on the next tick, in order.
- [ ] **Step 3: Run, verify fail.** `cargo test -p uw-spike-dlcollector` → FAIL.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run, verify pass.** `cargo test -p uw-spike-dlcollector` and the ignored
  `installed_build` → PASS.
- [ ] **Step 6: Commit.** `git commit -m "spikes: Deadlock collection tasks, task runner, fail-safe"`

### Task C4: In-game loading and main-thread timing (Spike 3)

**Files:**
- Modify: `spikes/ipc/src/lib.rs` (new `Msg` variants, appended at the end of the enum)
- Create: `spikes/input/src/module.rs` (portable command handling), `spikes/input/src/capture_win32_hook/plugin.rs` (`cfg(windows)` loader)
- Modify: `spikes/input/src/capture_win32_hook/win32.rs` (reader thread; input hooks only on `ModuleConfig`), `spikes/osr-host/src/main.ts` (send `ModuleConfig { input_hooks: true }` after the module's `Hello`)
- Create: `spikes/dl-collector/src/{timing.rs,plugin.rs}` (`timing.rs` portable gate + `cfg(windows)` hook; `plugin.rs` `cfg(windows)` exports)
- Modify: `spikes/dl-collector/Cargo.toml` (`[target.'cfg(windows)'.dependencies] minhook = "0.9"`)
- Test: `spikes/input/tests/module.rs`, `spikes/dl-collector/tests/timing.rs`, `spikes/ipc/tests/roundtrip.rs`

**Interfaces:**
- Consumes: `Collector`, `ChannelOut`, `Module::from_headers`, `SelfProcess` (C2/C3); `Producer` (C1).
- Produces (ipc):
  ```rust
  Msg::ModuleConfig { input_hooks: bool },                        // host → module, first reply to Hello
  Msg::LoadPlugin { path: String, channel: u32, args: String },   // host → module (Windows path)
  Msg::PluginLoaded { path: String, ok: bool, error: Option<String> }, // module → host
  ```
- Produces (input, portable): `pub enum ModuleAction { InstallInputHooks, LoadPlugin { path: String, channel: u32, args: String } }`;
  `pub struct ModuleCommands { /* hooks_installed */ }` with `fn on_msg(&mut self, m: &Msg) -> Vec<ModuleAction>`.
  `ModuleConfig { input_hooks: true }` yields `InstallInputHooks` once, ever; `false` yields nothing.
  The module installs no input hooks until it is told to (real-match runs send `false`).
- Plugin ABI (`cfg(windows)`, exported by `uw_spike_dlcollector.dll`, `extern "C"`):
  `uw_plugin_start(channel: u32, args: *const u8, args_len: usize) -> i32` (0 = started),
  `uw_plugin_present()`, `uw_plugin_stop() -> i32` (non-zero = refuses unload; the spike never
  unloads). The module loads it with `LoadLibraryW`, resolves the three names, calls start, replies
  `PluginLoaded`, then calls `uw_plugin_present` every 16 ms from its own thread.
- `uw_plugin_start`: creates `Producer` for the channel (a `ChannelOut` impl in `plugin.rs` wraps
  it: `commit` = one `Txn` + one info signal; `push_event` = `Throttle::admit` then `ring_push`), publishes `plugin_status="initializing"`,
  builds `Module`s for `client.dll`/`engine2.dll`/`tier0.dll` from `GetModuleHandleW`, runs
  `Collector::start`, resolves the main-loop caller, installs the QPC hook, publishes
  `"scanning"` (or `"error"` + reason). `uw_plugin_present`: if no pass ran for 5 s while
  scanning, publish `plugin_status="stalled"`; back to `"scanning"` on the next pass.
- QPC hook: MinHook on `ntdll!RtlQueryPerformanceCounter` (Wine's `kernel32!QueryPerformanceCounter`
  forwards there, so every caller passes through). The detour is a naked stub that passes the
  caller's return address to the handler without changing the stack:
  ```rust
  #[unsafe(naked)] unsafe extern "system" fn qpc_detour() {
      core::arch::naked_asm!("mov rdx, [rsp]", "jmp {h}", h = sym qpc_handler)
  }
  unsafe extern "system" fn qpc_handler(counter: *mut i64, ret: usize) -> i32; // calls the trampoline first
  ```
- Portable gate (`timing.rs`): `pub struct CallerFilter { pub immediate: u64, pub outer: Option<u64> }`;
  `pub struct TickGate { filter, interval_ticks, last_pass, in_pass }` with
  `fn should_run(&mut self, ret: u64, now: u64, outer: impl FnOnce() -> Option<u64>) -> bool` and
  `fn done(&mut self)`. `outer` is only evaluated when `ret == immediate` (it walks one frame with
  `RtlCaptureStackBackTrace`, for the case where the game reaches QPC through a tier0 wrapper).
  Pass interval: `PASS_INTERVAL_MS = 50` (20 Hz).
- Discovery mode: start args `discover=<seconds>` records, for every QPC call, the first 3
  return addresses and the thread id, then publishes the top 32 chains to
  `gep_internal.discovery` (add `discovery:4096` to the schema in `keys.rs`) as JSON
  `[{frames:[{module,rva}], per_sec, window_thread}]`. No tasks run in discovery mode.

- [ ] **Step 1: Write the failing tests.**
  - `ipc` `msg_roundtrip` extended with the three new variants.
  - `module_installs_input_hooks_only_when_told`: `ModuleConfig{false}` → `[]`;
    `ModuleConfig{true}` twice → `[InstallInputHooks]` then `[]`.
  - `module_forwards_load_plugin`: `LoadPlugin{..}` → `[ModuleAction::LoadPlugin{..}]` with the same fields.
  - `non_matching_caller_does_not_run_pass` (Review Focus 4): `ret != immediate` → `false`, and
    the `outer` closure is never called (it panics if called).
  - `pass_does_not_reenter`: `should_run` true, then before `done()` a matching call → `false`.
  - `passes_are_at_least_interval_apart`: matching calls at 0, 30 ms, 60 ms → true, false, true.
  - `outer_frame_must_match_when_set`: `outer: Some(X)`; closure returns `Some(Y)` → false, `Some(X)` → true.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-ipc -p uw-spike-input -p uw-spike-dlcollector` → FAIL.
- [ ] **Step 3: Implement** the portable parts, then the `cfg(windows)` parts. Check:
  `cargo check --target x86_64-pc-windows-gnu -p uw-spike-input -p uw-spike-dlcollector` → no errors.
- [ ] **Step 4: Run, verify pass.** Same test command → PASS. Re-run Spike 1's P1 and P5 once in the
  Hideout (osr-host, `--capture win32`) to show input still works with the deferred hook install.
- [ ] **Step 5: Find the main-loop caller (owner at the machine; run this after Task C5 Step 5,
  which builds `uw-gep`, and commit it separately).** Hideout, collector loaded with
  `discover=10` (via `uw-gep --args discover=10`, Task C5). Pick the chain that is on the game
  window's thread, inside a game module, and fires at about the frame rate. Write it as one or two
  masked patterns (`MAIN_LOOP_IMMEDIATE`, optional `MAIN_LOOP_OUTER`) with return-address offsets
  in `roots.rs`; add them to `installed_build` (exactly one hit each in the installed module).
  Not found → `Status::Error("main-loop caller not found")`.
- [ ] **Step 6: Commit.** `git commit -m "spikes: in-game plugin loading, MinHook QPC hook filtered by caller"`

### Task C5: Wine-side helper, Linux client `uw-gep`, event provider (Spike 3)

**Files:**
- Modify: `spikes/ipc/src/lib.rs` (relay variants, appended), `spikes/inject/src/launcher.rs`
  (`GameSession`), `spikes/inject/src/bin/uw-spike-inject.rs` (use `GameSession`),
  `spikes/inject/install.sh` (also build and install the collector DLL and helper exe),
  `spikes/osr-host/src/main.ts` (flag `--gep <args>`: after `ModuleConfig`, send `LoadPlugin`)
- Create: `spikes/gep-helper/` (crate `uw-spike-gep-helper`, Windows bin `uw-gep-helper`):
  `src/lib.rs` (portable `relay`), `src/main.rs` (`cfg(windows)` consumer + TCP)
- Create: `spikes/gep-host/` (crate `uw-spike-gep-host`, Linux bin `uw-gep`): `src/provider.rs`,
  `src/status.rs`, `src/main.rs`
- Test: `spikes/gep-helper/tests/relay.rs`, `spikes/gep-host/tests/{provider.rs,status.rs}`,
  `spikes/inject/tests/launcher.rs`

**Interfaces:**
- Consumes: `Consumer`, `InfoReader`, `RingReader` (C1); `LoadPlugin`, `ModuleConfig`,
  `PluginLoaded` (C4); `write_msg`/`read_msg` (ipc).
- Produces (ipc, helper → `uw-gep` over TCP `127.0.0.1:48611`, env `UW_SPIKE_GEP_PORT`):
  ```rust
  Msg::GepConnected { channel: u32, pid: u32, version: u32 },
  Msg::GepInfo { category: String, key: String, value: Option<String>, ts: u64 },
  Msg::GepEvent { name: String, data: String, ts: u64 },
  Msg::GepLost { events: u64 },
  Msg::GepDisconnected,
  ```
- Helper: `uw-gep-helper <channel>` waits for the channel's `connect` event, attaches, sends
  `GepConnected` then the full info snapshot, then relays changes and events as they are
  signalled. On `disconnect` it sends `GepDisconnected` and waits for `connect` again. It retries
  the TCP connection every 1 s, and sends a fresh snapshot after each reconnect (Review Focus 3).
  `pub fn relay(changes: Vec<InfoChange>) -> Vec<Msg>`: non-UTF-8 values are dropped and logged.
- inject: `pub struct GameSession { pub pid: i32, pub env: Vec<(String,String)>, pub prefix: PathBuf, pub proton: PathBuf }`;
  `GameSession::find(exe: &str, pid: Option<i32>) -> Result<GameSession, String>` (the lookup that
  `uw-spike-inject` does today, moved here); `GameSession::wine(&self, program: &Path, args: &[OsString]) -> Command`.
- `uw-gep watch [--channel N] [--args S] [--no-inject] --jsonl out.jsonl`: default channel `1`.
  Without `--no-inject` it listens on 48610, injects `uw_spike_input.dll`, answers the module's
  `Hello` with `ModuleConfig { input_hooks: false }` and `LoadPlugin { collector dll, channel, args }`.
  It always starts the helper with `GameSession::wine` and listens on 48611. `--no-inject` is for
  Hideout runs where osr-host owns 48610 (`npm start -- --capture win32 --gep ""`).
  JSONL: one line per emission, `{"t_ms":…, "kind":"info"|"event"|"status", …}`. On Ctrl+C it
  prints the status table.
- Provider (portable, the "above the transport" layer):
  ```rust
  pub enum Emission { Info { feature: String, category: String, key: String, value: Option<String> },
                      Event { name: String, data: String } }
  pub struct Provider { /* last values, local roster counters, match state */ }
  impl Provider { pub fn on_info(&mut self, category: &str, key: &str, value: Option<&str>) -> Vec<Emission>;
                  pub fn on_event(&mut self, name: &str, data: &str) -> Vec<Emission>; }
  ```
  Info passes through (feature = category). Events: the local roster row's `kills`/`deaths`/
  `assists` +1 → `kill`/`death`/`assist`, compared only within one `match_id`; phase becomes
  `GameInProgress` → `match_start` once per `match_id`; phase leaves `GameInProgress`/`PostGame`/
  `PostGame_PlayOfTheGame` (or is cleared) after a `match_start` → `match_end`; collector `clock` →
  `match_clock`; `game_paused` and `tower_destroyed` pass through. `game_info.steam_id` is the
  local roster row's `steam_id` (read from memory, not derived).
- Status table (`status.rs`): one row per spec §6.4 key and event, value `supported` (seen at least
  once), `disabled: <reason>` (from `disabled_tasks`), `error: <reason>`, or `unavailable`.

- [ ] **Step 1: Write the failing tests.**
  - `relay_snapshot_then_changes`: 2 keys in the snapshot → 2 `GepInfo`; a `None` value relays as `value: None`.
  - `relay_drops_non_utf8_value`.
  - `game_session_wine_command_uses_game_env`: a fake environ with `WINEPREFIX` and `WINEFSYNC=1`
    → the command has both and no inherited `WINESERVER`.
  - `kill_delta_emits_kill_event`, `death_and_assist_deltas_emit_events`.
  - `new_match_resets_counters_without_events`: `match_id` changes and kills drop 5 → 0 → no event.
  - `match_start_once_per_match`, `phase_transition_emits_match_start_and_end`.
  - `clock_event_becomes_match_clock`, `local_steam_id_comes_from_local_roster_row`.
  - `status_marks_disabled_and_unavailable`: `disabled_tasks=[{"task":"clock","reason":"slow"}]`
    and no `team_score` ever → `match_clock: disabled: slow`, `team_score: unavailable`.
- [ ] **Step 2: Run, verify fail.** `cargo test -p uw-spike-gep-helper -p uw-spike-gep-host -p uw-spike-inject` → FAIL.
- [ ] **Step 3: Implement.** `cargo check --target x86_64-pc-windows-gnu -p uw-spike-gep-helper` → no errors.
- [ ] **Step 4: Run, verify pass.** Same test command → PASS.
- [ ] **Step 5: End-to-end without the game.** Extend `spikes/inject/tests/wine_e2e.rs` (ignored):
  in the scratch Proton prefix, inject the module into `wine_sleeper`, send `LoadPlugin` with a
  test-only collector start arg `selftest=1` (the collector publishes `plugin_status="selftest"`
  and one event `selftest`, without resolving roots), start the helper, and assert `uw-gep`
  receives both. Then kill and restart the helper: it gets the snapshot again.
- [ ] **Step 6: Commit.** `git commit -m "spikes: Wine-side GEP helper, uw-gep client and event provider"`

### Task C6: Live runs: accuracy, latency, fail-safe, cost, robustness (Spike 3)

**Files:**
- Create: `spikes/gep-host/src/verify.rs` and subcommand `uw-gep verify --jsonl out.jsonl --match-id <id>`:
  fetches `https://api.deadlock-api.com/v1/matches/{match_id}/metadata` and compares final
  per-player kills/deaths/assists, hero, team and match id, printing a per-field
  `OK`/`MISMATCH`/`MISSING` table.
- Create: `docs/spikes/03-deadlock-memory.md` (from `docs/spikes/TEMPLATE.md`)
- Modify: `spikes/README.md` (how to run Spike 3; replace the outdated safety rules with the
  current Global Constraints)

All steps need the owner at the machine. Record every result in `docs/spikes/03-deadlock-memory.md`.

- [ ] **Step 1: Hideout and bot match, everything on.** osr-host `--capture win32 --gep ""` +
  `uw-gep watch --no-inject`. Expected: `plugin_status="scanning"`, phase, roster (12 in the bot
  match, one local), items and clock arrive; Ctrl+Tab edit mode still works. Check the bot match
  by hand against the end-of-match scoreboard.
- [ ] **Step 2: Cost.** Bench procedure in the Hideout, collector off vs on (both with the module
  injected and input hooks off): `uw-spike-bench compare`. Expected: `PASS`. Also record the
  median and max pass time from `gep_internal.pass_stats`.
- [ ] **Step 3: Accuracy, collector only (owner decision).** At least 3 real matches with
  `uw-gep watch` (no Vulkan layer, no Electron host, input hooks off). Run `uw-gep verify` on each.
  Expected: no `MISMATCH`.
- [ ] **Step 4: Latency.** In a bot match, screen-record next to `watch`; for 5 kills compare the
  kill-feed time with the `kill` event's `t_ms`. Expected: ≤ 1 s each.
- [ ] **Step 5: Fail-safe (Review Focus 1).** `uw-gep watch --args corrupt=PlayerDataGlobal_t.m_iPlayerKills`,
  then `corrupt=C_BaseEntity.m_iTeamNum`. Expected: the roster task is disabled (`invalid: …`) or
  publishes no wrong value; compare against Step 1 values. Any wrong value published is a Spike 3
  failure: stop and report.
- [ ] **Step 6: Restarts (Review Focus 3).** During a Hideout session kill and restart `uw-gep`
  and the helper. Expected: the full snapshot arrives again, the game keeps running.
- [ ] **Step 7: Robustness across updates.** The 6801 update already happened once (see
  "Evidence before planning"). After the next Deadlock patch, rerun the ignored `installed_build`
  test and Step 1 with **no code changes**. Record which patterns and fields survived and the
  diff of the two schema dumps. If no patch lands within the spike window, record that.
- [ ] **Step 8: Timeboxed features (2 days, owner decision).** For `game_mode`, `game_paused`
  (if not already from the clock task), `team_score`, `match_outcome`, `tower_destroyed` and
  `incoming_damage`: search `dumps/schema-6801.json` for candidate class.fields, add a task for each
  candidate with a test in `tests/tasks.rs` in the same style as C3, and check it live. Anything
  not found or not matching the game's display at the end of the timebox is recorded with its
  alternative source (console log, local files, deadlock-api.com) or as `unavailable`. Never
  derive or estimate a value.
- [ ] **Step 9: Commit.** `git commit -m "spikes: Spike 3 live results (accuracy, latency, fail-safe, cost)"`

### Task C7: Spike 3 findings and spec record

**Files:** Modify `docs/spikes/03-deadlock-memory.md`, `docs/superpowers/specs/2026-10-20-underwolf-design.md` (§6.3)

- [ ] **Step 1: Write the findings, including the feature-by-source table** that spec §8.3 asks
  for. Columns: Feature/key | Source (memory / console log / deadlock-api.com / unavailable) |
  Resolution (schema name / pattern) | Accuracy result | Survived patch | Notes. One row per §6.4
  feature and event. List the Linux differences used (module timer calls `present`; Wine-side
  helper relays the channel; `ReadProcessMemory` on self for safe reads) and the main-loop caller
  that was chosen.
- [ ] **Step 2: Record the hook engine in spec §6.3:** "One inline hook engine for every in-game
  hook: MinHook. Spike 3 uses it for QPC; the input hooks move to it in milestone 1."
- [ ] **Step 3: Gate.** If phase, roster, kill/death/assist or match_clock can't be read
  live, or any wrong value was published, stop and report to the owner.
- [ ] **Step 4: Commit.** `git commit -m "docs: spike 3 findings (in-game collector)"`

### Task 17 (revised by Amendment C): Spike summary and owner review

**Files:** Create `docs/spikes/SUMMARY.md`

- [ ] **Step 1: Write the summary.** One line per spike (Linux only): verdict, chosen mechanism,
  link to the findings doc. Then "Decisions needed from owner", collected from every findings doc
  (at minimum: AMD verification status, Proton-version fragility of injection and input, any
  Spike 3 features marked unavailable). Then "Not scheduled": Windows (Vulkan layer on Windows,
  DX11 hook, Windows installer, code-signing).
- [ ] **Step 2: Propose the milestone 1 plans, in order.** Each one names the spec section it
  implements and what the spikes already proved:
  1. **Injection** (spec §6.3 "Getting in"): the helper per bitness, the bootstrap DLL loaded
     with a thread-targeted `SetWindowsHookEx(WH_GETMESSAGE)` (`WH_CALLWNDPROC` fallback) plus
     wake-up messages, self-pinning, and load-overlay on command. Replaces the spike's remote
     `LoadLibraryW`. Includes the known gap: the module reconnects after a client restart.
  2. **Communication** (spec §6.3 "Client ↔ in-game channel"): the duplex named pipe of fixed-layout
     messages (hello first), named auto-reset events plus a shared bitmask as doorbells, and the
     shared state block, all owned by the Wine-side helper, which relays them to the Linux host.
     Replaces the spike's localhost TCP to the module; the Spike 3 data-channel relay folds into
     the same helper.
  3. **Input layers** (spec §6.3 "Input, all of Overwolf's layers"): every hook on MinHook (moving
     C3's IAT hooks), thread keyboard/mouse hooks, key-state, raw-input read/registration with
     exact restore, cursor position/clip/visibility/shape, DirectInput, GameInput, the hotkey
     table pushed by the client, in-module hit-testing, and `passThrough`/`noPassThrough`/
     `passThroughAndNotify`.
  4. **Dirty-rect frames** (spec §6.3 "Widget frames"): per-widget control block (size, frame
     counter, last-consumed frame, dirty rect, new/consumed flag) in Linux shared memory next to
     today's slots; the producer grows the dirty rect until the consumer catches up and the layer
     copies only dirty rows. Target: bring Spike 2's full-cost result under the 3% gate.
  5. Then the Deadlock provider (from Spike 3), the app runtime, the launcher and the catalog.
- [ ] **Step 3: Commit.** `git commit -m "docs: spike summary"`
- [ ] **Step 4: Hand the summary to the owner for review** before any milestone plan is written.
