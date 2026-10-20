# Underwolf technical spikes

Prototypes that answer the three questions in §8 of
`docs/superpowers/specs/2026-10-20-underwolf-design.md`. The code here is throwaway quality, but
it is tested. The findings in `docs/spikes/` are the deliverable.

Plan: `docs/superpowers/plans/2026-10-20-underwolf-spikes.md`.

## Safety rules (non-negotiable)

- Underwolf must never write to game memory or alter game behavior. Anything inside the game
  process is read/draw-only.
- Never write to game memory, never inject code for data collection, never alter game behavior.
- No reverse engineering of the Overwolf client, and no redistribution of Overwolf code or apps.
- If a spike fails, stop and bring the finding back to the owner before re-planning. Do not
  quietly add a windowed fallback or a memory-writing workaround.
- If the Deadlock provider can't get a value, the event/field is simply not emitted. No
  fabricated or estimated values.
- Textures stay on the GPU (no CPU copies on the frame path). GPU-to-GPU copies are allowed.
- Overlay testing in Deadlock happens in the Hideout or bot matches. Only the read-only memory
  reader runs in real matches (the owner has accepted that VAC risk).

## Crates

| Path | What it is | Spike |
|---|---|---|
| `common/` | target-process guard, frame-time stats, `uw-spike-bench` | all |
| `vk-layer/` | Vulkan implicit layer `VK_LAYER_UNDERWOLF_spike` | 2, 1 |
| `ipc/` | socket protocol between the Electron host and the layer | 2, 1 |
| `osr-bridge/` | napi-rs addon used by the Electron host | 2, 1 |
| `osr-host/` | Electron offscreen-rendering host (not a Cargo crate) | 2, 1 |
| `dmabuf-consumer/` | standalone dmabuf import check | 2 |
| `input/` | edit-mode input candidates | 1 |
| `dl-reader/` | read-only Deadlock memory reader, `uw-dlreader` | 3 |
| `dx11-hook/` | Windows DX11 present hook + injector | 2 |

How to run each spike is added to this README by the task that builds it.

## Vulkan layer (`vk-layer/`)

```sh
./spikes/vk-layer/install.sh     # build + install as a user implicit layer
./spikes/vk-layer/uninstall.sh   # remove it
```

- Loads into every Vulkan process, but does nothing unless the process is `deadlock.exe` or
  `UW_SPIKE_FORCE_TARGET=1` is set.
- `UW_SPIKE_MODE=passthrough|quad|composite` (default `passthrough`).
- `UW_SPIKE_LAYER_DISABLE=1` stops the loader from loading it at all.
- Frame times: `~/.cache/underwolf-spike/frames-<pid>-<mode>-<segment>.csv`, flushed every second.
- Runtime mode switch: write a mode to `~/.cache/underwolf-spike/control`. Every write starts a
  new CSV segment. Files older than the process are ignored.
- Check it loaded: `VK_LOADER_DEBUG=layer UW_SPIKE_FORCE_TARGET=1 vkcube`. In Deadlock, set
  Steam launch options to `PROTON_LOG=1 VK_LOADER_DEBUG=layer %command%` and read
  `~/steam-1422450.log`.

## Bench procedure

Use this procedure for every overlay performance number.

1. Deadlock Hideout, same spawn position and view every run, same graphics settings.
2. VSync off, FPS cap off.
3. 3 runs × 60 s per mode, alternating modes: A B A B A B.
4. With the game running (layer installed), run `./spikes/bench/ab.sh passthrough quad` (or
   `passthrough composite`). It switches modes A B A B A B through the control file, 65 s
   each, and runs the comparison below on the new segments. Stand still the whole time.
5. Manual comparison of any CSVs:

   ```sh
   cargo run -p uw-spike-common --bin uw-spike-bench -- compare <A runs...> -- <B runs...>
   ```

   The tool drops the first 5 s of each run, averages the runs, and prints `PASS` when average
   FPS loss is ≤ 3.0% **and** the p99 frame-time increase is ≤ 3.0%. Exit code: 0 PASS, 1 FAIL,
   2 usage error.
