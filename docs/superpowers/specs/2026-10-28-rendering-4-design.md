# Rendering 4 — dirty-rectangle widget frames

**Date:** 2026-10-28

**Status:** Approved by the owner on 2026-10-28, following review of draft commit
`9adc270`. Implementation planning is authorized; implementation has not started.

**Starting point:** `main` at `7023ccc`, after local integration of input 3b.

**Builder check:** A fresh read-only reviewer reported no important unanswered
planning questions on 2026-10-28; the owner subsequently approved the design.

## Intent

Implement milestone 1, roadmap item 4 from [the spike summary](../../spikes/SUMMARY.md):
replace whole-widget frame copies with accumulated dirty rectangles, and establish
the production Linux renderer. Widgets must remain visually correct when paints
outpace the game, frames are skipped, either process restarts, or a widget changes
size. Measure the whole overlay against the existing 3% performance budget.

The player-facing result is the same in-game Chromium widgets, with less copying
when only a small region changes. The collector, app runtime, launcher and catalog
come after this phase. GameInput remains explicitly deferred under the 3b decision.

## Inherited decisions and scope

The parent design and roadmap already choose:

- Linux/Proton first; native Linux Vulkan compositing inside the game frame.
- Electron + TypeScript and Rust. CPU widget pixels are allowed on NVIDIA.
- Per-widget shared pixels and a mutex-protected control block containing size,
  frame and consumed counters, accumulated damage and new/consumed state.
- Optimize changed regions first, per-widget frame rates next, GPU shared textures
  after the AMD check. The full Chromium + copies + layer cost owns the budget.
- The injected Windows module remains the sole authority for hit-testing and input
  capture. Native rendering installs no input hooks and uses no spike injector.
- No game data/code modifications, game-function calls or device-wide idle waits.
- Production code must not depend on `spikes/*`.

Included in phase 4:

- Production frame contracts, Linux shared-memory transport, Node bridge and
  Vulkan layer, ported selectively from the working spike implementation.
- Dirty-region CPU writes and GPU uploads, synchronization, bounded backpressure,
  resource reclamation, resize/reconnect recovery and explicit diagnostics.
- A small Electron acceptance host with controlled widgets and the existing
  production input session. This is a fixture, not the app runtime.
- Repeatable pixel, lifecycle, Vulkan-validation and full-overlay cost checks.

Excluded:

- Windows rendering/DX11, a Windows release, 32-bit support and additional games.
- GPU shared-texture/dmabuf enablement, automatic renderer selection and AMD
  acceptance without hardware. Those remain later work, not inferred results.
- Manifest/package loading, `overwolf.*`, persistent layouts, production editing
  controls, provider migration, launcher/catalog and distribution installers.
- New input APIs or isolation claims. Preserve accepted 3b behavior and its limits.

## Approaches

**Carry the existing shared-buffer/control-block decision into production.**
Accumulate damage until consumption, retain current pixels while the GPU reads,
and coalesce subsequent paints on the producer. This follows the owner's existing
architecture and optimization order. The one outstanding upload per widget
described below is an author-proposed implementation choice, not a new owner ruling.

An immutable multi-slot snapshot pipeline could permit more outstanding uploads,
but introduces more per-slot damage history and GPU-lifetime accounting. It is a
reasonable later optimization if measurements show the simpler protocol is too
slow. It must not silently replace the control-block contract in this phase.

Keeping full-frame copies would preserve the measured spike cost; switching to GPU
shared textures first would bypass the agreed order and the unresolved NVIDIA
problem. Neither satisfies this roadmap item.

## Current implementation and required migration

- `spikes/osr-host/src/main.ts` ignores `dirtyRect`, calls `image.toBitmap()` for
  every paint and starts an asynchronous full-frame send. All widgets use 60 FPS.
- `spikes/osr-bridge` writes full bitmaps into three memfd slots. A timeout clears
  a slot's busy flag without proving that the GPU has finished reading it.
- `spikes/vk-layer` imports shared pages with `VK_EXT_external_memory_host` and
  uploads entire widgets with `vkCmdCopyBufferToImage`. Replaced or non-composited
  frames can be acknowledged before upload; mappings are intentionally leaked.
- The spike has valuable present/semaphore plumbing, premultiplied blending,
  sRGB handling and fence-based deferred destruction. Preserve those mechanisms
  after review, rather than depending on spike crates or copying obsolete input.
- Production `uw-overlay` is the Wine-side input DLL. It is not the Linux Vulkan
  layer and must not become one. `uw-host` already provides discovery, supervision
  and input consumption contracts; the render bridge is a separate native path.

Suggested boundaries, with names left to the implementation plan: `uw-frame-proto`
for portable validation/state, `uw-frame-channel` for Linux transport and shared
memory, `uw-render-bridge` for Node integration, `uw-vk-layer` for the Linux layer,
and `tools/render-fixture` for Electron acceptance. Keep Linux dependencies
target-gated so existing Windows/Proton builds continue to work.

## Frame and damage contract

Identity includes the render session, widget instance/generation and allocation
generation. A resized or replaced widget gets a new allocation generation, even
when its numeric ID, dimensions or driver handles happen to match old values.
Routing identities must map explicitly to render identities; input configuration
revisions and render frame counters are separate concepts.

Frame dimensions and dirty rectangles are physical image pixels. Layout position,
display scaling and logical widget coordinates are converted explicitly; never
assume `BrowserWindow` bounds equal bitmap dimensions. Keep BGRA8 premultiplied
pixels and established sRGB/linear swapchain behavior for this path.

The control block has a checked version/header, identity, width, height, stride,
pixel allocation extent, produced sequence, last-consumed sequence, dirty bounds
and publication/reader state, protected by a process-shared robust mutex. Freeze
the exact layout, initialization handshake and platform ABI in the plan before
writing shared-memory code. Never cast unvalidated received bytes into a mutex.

Use one bounding rectangle, not an unbounded list. For each accepted paint, union
its damage with all damage not yet consumed. Empty damage is a no-op; negative,
overflowing or inconsistent metadata is rejected. Bounds arithmetic and all row
offsets are checked. Trusted Electron rectangles are normalized once into validated
frame coordinates before entering the native contract.

The first publication, a new allocation or a fresh consumer image requires the
whole frame. Partial updates are legal only after that baseline. A layout-only
move or visibility change needs no new pixel upload.

Counters are monotonic within a generation; wrap is a generation reset, not reuse.
The consumed counter means the matching GPU upload has completed. An IPC receipt,
queued command, superseded frame or timer expiry never advances it.

### Producer and consumer sequence

1. The Electron producer owns a bounded latest-image reference and a union of
   damage not yet copied into shared pixels. New paints replace that reference
   and extend damage, including while a previous upload is in flight.
2. When the shared buffer is not GPU-owned, the producer extracts the needed
   region from the newest complete image, tries the metadata mutex, rechecks
   identity/state and copies only the dirty row spans into shared pixels. Commit
   the sequence and accumulated shared damage after the pixels are ready.
3. A ready, unclaimed publication may be updated again under the same mutex.
   Its dirty bounds continue growing from the last consumed baseline. The shared
   bitmap retains valid pixels outside each incoming patch.
4. The consumer uses `try_lock`, claims one ready publication and snapshots its
   sequence/damage. Once claimed, shared pixels are immutable until the upload
   fence signals. Release the metadata mutex before Vulkan work; never hold it
   while waiting for the GPU, IPC or the producer.
5. Record an upload for that dirty rectangle from the imported shared buffer into
   the persistent widget image. Keep drawing the previous image while no safe
   update is available. One upload per widget may be outstanding.
6. After the matching GPU fence signals, advance consumption and release the
   pixel buffer for writing. A coalesced completion notification wakes the
   producer to flush retained damage even if the widget never paints again.
   Polling/retry outside the present path repairs lost/coalesced notifications.

For example, if rectangle A was published and rectangle B changes during its GPU
upload, completion of A releases only A's publication. The retained newest image
and B's pending damage still cause a second publication. If both A and B changed
before the consumer claimed either, its next upload covers their union.

Notifications are hints; validated shared state is authoritative. A timer may
report a stalled widget, but cannot authorize overwriting GPU-owned memory.
Disconnect abandons that generation. A new connection uses fresh backing storage;
the consumer keeps old mappings alive until its old submissions are complete.

### Electron contract qualification

Keep the repository's pinned Electron 44.9.1 for this work. Its `paint` docs call
the `NativeImage` a whole frame but also describe dirty-area buffering. Before
relying on deferred image extraction, a focused fixture must demonstrate that a
retained paint image supplies a complete, stable snapshot across later paints.
Test two disjoint changes and delayed extraction, plus the actual device scale.

If retained images cannot supply stable complete pixels, the producer must keep
one owned complete backing image updated from every paint's dirty rows. Do not
drop damage or assume uncopied bytes are valid. Record the additional copy and
measure its cost; this is a correctness fallback within the CPU path.

Use documented image extraction, optionally cropping the accumulated region
before `toBitmap()`, only after verifying dimensions, row stride and colors.
`getBitmap()` is a deprecated alias, not a zero-copy workaround. Count extraction
and cropping costs separately: reduced shared-memory/GPU bytes do not prove that
Chromium readback or `NativeImage` conversion has become partial.

## Transport, ownership and failure behavior

Use a versioned local Unix socket for handshake, descriptor passing and bounded
control notifications; widget metadata and pixels live in memfd-backed shared
memory. The Wine helper is not a pixel relay. Keep render protocol versions
separate from the already accepted pipe/input/data versions.

Bind an endpoint to one explicitly selected Linux game process and its process
incarnation, in a private per-user directory visible inside pressure-vessel.
Validate peer credentials and session identity. Another target process must be
rejected or assigned a distinct session; it must not evict the active game. The
plan must verify the chosen path inside Proton rather than assume runtime-directory
visibility. Non-target Vulkan clients, including Electron's GPU process, pass through.

Author-proposed initial limits: 32 widgets, 4096 pixels per side, checked BGRA
stride, and 256 MiB of shared pixel allocations per session, including allocations
retired but still GPU-referenced. Bound control messages, descriptors, pending
notifications and retired GPU resources too. Reject unsupported requests clearly;
do not silently truncate, allocate without limit or wait for a game frame to free
capacity. The plan may lower these limits with a documented reason.

Control metadata uses recoverable process-shared locking. Busy means skip and
retry; owner death or invalid publication state means invalidate the allocation
and request a fresh full baseline. Do not make damaged state consistent and then
consume its pixels. Validate file extents, alignment and immutable allocation
dimensions before importing. Keep metadata out of the GPU's pixel import range.

All socket I/O, descriptor handling and retries run outside present. Present must
not block on a shared-memory lock, socket or host progress. If the layer's own
submission slot is busy, skip its overlay submission and preserve the game's
original present wait semaphores. If submission succeeds, chain waits/signals
correctly. No `vkDeviceWaitIdle`, queue-wide idle or unbounded fence wait.

Shared mappings, imports, widget images and descriptors have explicit owners.
Retire them together and reclaim only after all submissions referencing them
finish. Driver handle equality is not resource identity. Repeated resize/reconnect
must reach a memory plateau once fences drain, replacing the spike's intentional
mapping leak. A missing completion cannot be converted into permission to free a
resource that the GPU might still access.

A failed widget upload leaves its last valid image, or hides that widget when no
valid image exists, and reports the failure. Unsupported import capability leaves
the game running with a clear overlay-unavailable result. No windowed fallback.
Connection loss removes stale widgets and edit-mode dimming. Reconnect requires
fresh generations and full baselines for static widgets as well as animated ones.
Late completion, layout or frame notifications cannot affect a newer generation.

## Rendering and existing input

Port only native compositing and its required loader/device/swapchain handling.
Do not port XInput2/Wine input filters, the C3 TCP listener, plugin loading, old
hotkey logic or the old remote-thread injector into the render stack.

The acceptance host uses the production injection/host APIs and input v2. Actual
capture state drives dimming; there is no decorative second cursor in production
input mode. Events are acknowledged only after real renderer consumption, using
3b's epoch and cancellation barriers. Pixel-upload completion is not input-event
consumption and cannot answer an input lease challenge.

Render availability is part of the fixture's capture readiness: do not request or
accept takeover for invisible/unavailable widgets. On loss of the render session,
the fixture stops acknowledging input and requests release through the current
production channel; 3b's game-local lease remains the backstop. Recovery restores
configuration and pixels, never previous input ownership. Exercise renderer stall
and independent native-layer disconnect explicitly.

Use the existing visibility/configuration controls to keep takeover unavailable
until the layer reports ready and initial widget uploads complete. A disconnect
can race a physical toggle; in that case request release immediately and retain
the existing bounded game-local lease guarantee. Do not claim an atomic new
native render/input readiness handshake without implementing and testing one.

The fixture supplies animated, static and small-damage widgets with deterministic
colors, transparency and a typing/clicking target. Positions, scale and stacking
must agree with the input table. Preserve 3b's three passthrough modes and ability
to exit capture. Fixture controls are not production layout editing or persistence.

## Validation and acceptance

### Automated correctness

- Portable frame/damage state tests: disjoint/overlapping unions, clipping policy,
  malformed dimensions/stride/offsets, first-frame baseline, sequence/generation
  resets and update-during-consumption schedules. Compare reconstructed output
  with a reference full image, not only with the implementation's metadata.
- Real cross-process transport/mutex tests: descriptor and version validation,
  busy/dead owner, producer/consumer exits, late completion, replacement, bounded
  queues and memory reclamation. Include a publication interrupted during writing.
- Electron 44.9.1 image-contract qualification and measured extraction; static
  initial draw, two separated damage regions, transparent clears and retained
  pending pixels flushed without another paint. Cover scale 1 and non-unit scale
  where the fixture can set and observe it.
- Vulkan smoke/pixel readback under core and synchronization validation. Exercise
  dirty copies, initialized image preservation, sRGB/alpha, resize, reconnect,
  non-composite transitions and multiple swapchain images. Delay GPU completion
  deliberately and prove that producer writes cannot race the copy.
- Repeated host/allocation churn with drained-fence resource counts, and rejection
  of a second client attempting to take over the active session.
- Existing portable production workspace and relevant Node input receiver tests;
  production Linux/Windows checks and strict lint/format gates scoped to changed
  code. Preserve source-specific 3b native evidence; new claims require new runs.

### Live Linux/Proton acceptance

Use the current Deadlock build and record its version, Proton, desktop, GPU/driver,
resolution, scale, artifact hashes and exact source revision. Confirm the production
layer loaded and the spike layer/input path is inactive for these runs. Preserve
existing spike installations and evidence; use explicit per-run selection.

Verify all widgets and colors in the game, small changes without trails or missing
pixels, clearing pixels to transparent, static first frame, move/hide/show, resize,
host stop/restart, game restart, alt-tab, fullscreen/windowed and resolution change.
Repeat the spike's previously unverified same-handle/widget-replacement visual
case. Check real typing/clicking and input recovery after fixture/render loss,
without automatic recapture. Record automated observations and owner observations
separately; do not label a manual requirement passed from logs alone.

### Whole-overlay performance gate

The acceptance comparison remains:

- A: production layer loaded but inactive, no Electron host or input helper running.
- B: production layer, Chromium, bridge, shared-memory copies and production input
  session running; three visible widgets matching the spike workload: one 400x300
  animation at 60 FPS and two 300x200 static widgets. No exclusive capture during
  these idle-overlay traces. Input-hook baseline overhead therefore counts in B.

Run at least three matched 60-second A/B pairs, alternating order, with warmup,
the same focused scene/settings and no active frame cap. Repeat the normal and
owner-max-settings scenarios if available. Report FPS loss and p99 frame-time
increase with the original formulas and per-run traces. The gate is at most 3%
degradation on both metrics per scenario, evaluated from the paired aggregate;
publish each pair as well so instability is visible. Mark disturbed/unfocused,
capped or interrupted traces invalid instead of averaging them into a pass.

Use a fresh game process for each A and B condition so A never inherits B's pinned
input DLL or installed detours. Match the build, launch configuration, scene and
sampling method across those processes. Merely stopping the helper does not
establish an uninjected baseline. Restore any temporarily changed frame cap or
other game setting after the coordinated benchmark.

Additionally compare full-copy and dirty-copy modes in the same production build
and run deterministic small-damage/full-damage workloads. Record actual extracted,
CPU-written and GPU-uploaded bytes, coalesced paints, consumer lag and host CPU
time. A full-surface animation may gain little: dirty rectangles are not assumed
to improve pixels that all change. Do not add lower frame rates or GPU textures
to rescue this gate without a separate scope decision under the agreed order.

If the full-overlay gate still fails, record the result and identify the measured
cost before proposing the next optimization. Functional transport checks do not
make this performance requirement complete. GPU-bound and AMD coverage stay
explicitly unverified when hardware cannot provide them. Live benchmarks require
a coordinated game session; no game settings/processes are changed during design.

## Plan handoff and remaining builder choices

Implement in dependency order: image-contract qualification and portable state;
shared-memory/transport lifecycle; producer bridge; Vulkan port and dirty uploads;
fixture/input integration; automated and live/performance acceptance. These are
work packages in one phase, not independently accepted completed phases.

The implementation plan decides exact crate names, encoded fields/offsets, Rust
types, lock ABI verification, capability negotiation, descriptor transport, shader
build tooling, fixture packaging and instrumentation. It must show byte bounds,
allocation alignment and failure transitions before coding. Keep existing pinned
libraries unless a demonstrated blocker requires an explicit change.

The detailed protocol, single outstanding upload, resource limits and acceptance
implementation originated as author proposals and are included in the owner's
2026-10-28 design approval. No implementation, coordinated live-game test or
performance pass is implied by that approval.

## Documentation checked

Context7 resolution was attempted for Electron and Vulkan on 2026-10-28, but both
returned the teamspace's monthly spending-limit error. Official sources were used
as the fallback:

- [Electron 44.9.1 paint/invalidate contract](https://github.com/electron/electron/blob/v44.9.1/docs/api/web-contents.md#event-paint)
  and [NativeImage extraction](https://github.com/electron/electron/blob/v44.9.1/docs/api/native-image.md#imagetobitmapoptions):
  whole-frame image plus dirty metadata, explicit repaint, copied bitmap output,
  and the deprecated `getBitmap` alias. The contradictory dirty-buffer wording is
  why the focused image qualification is a required first task.
- [Vulkan buffer/image regions](https://docs.vulkan.org/refpages/latest/refpages/source/VkBufferImageCopy.html)
  and [copy validity](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdCopyBufferToImage.html):
  rectangular offsets/extents, row stride and transfer restrictions.
- [Vulkan synchronization](https://docs.vulkan.org/spec/latest/chapters/synchronization.html)
  and [host-pointer import lifetime](https://docs.vulkan.org/refpages/latest/refpages/source/VkImportMemoryHostPointerInfoEXT.html):
  completion and host allocation lifetime must be established explicitly.
- [Linux robust-mutex recovery](https://man7.org/linux/man-pages/man3/pthread_mutex_consistent.3.html):
  owner death requires recovering the protected state, not blindly trusting it.
