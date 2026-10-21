# Underwolf technical spikes

Prototypes that answer the three questions in §8 of
`docs/superpowers/specs/2026-10-20-underwolf-design.md`. The code here is throwaway quality, but
it is tested. The findings in `docs/spikes/` are the deliverable.

Plan: `docs/superpowers/plans/2026-10-20-underwolf-spikes.md`.

## Safety rules and constraints (non-negotiable)

Copied from the plan's Global Constraints and Amendment C ("Constraints added by this amendment").

- Underwolf must never change the game's data or code or alter game behavior. Anything inside
  the game process is read/draw-only (drawing, input routing in edit mode, and read-only data
  collection).
- Never change the game's data or code, never call game functions, never alter game behavior.
  Hooks on OS, graphics, input and timing functions are allowed (spec §6.3). Deadlock data is
  collected inside the game by the injected module, as Overwolf does (owner decision 2026-10-21).
- No redistribution of Overwolf code or apps. (The clean-room / no-reverse-engineering rule was
  lifted by the owner on 2026-10-21; see spec §3.)
- If a spike fails, stop and bring the finding back to the owner before re-planning. Do not
  quietly add a windowed fallback or a memory-writing workaround.
- If the Deadlock provider can't get a value, the event/field is simply not emitted. No
  fabricated or estimated values. If layouts don't match, stop emitting rather than emit wrong
  data. A bad read inside the game must never crash it: every read is validated (mapped, bounded)
  before it is dereferenced.
- Performance gate: the overlay costs under ~2–3% FPS, with no visible stutter. Measured as
  average FPS loss ≤ 3.0% **and** p99 frame-time increase ≤ 3.0% (bench procedure below). The
  collector runs on the game's main thread, so nothing it does may block that thread on another
  process.
- Textures stay on the GPU (no CPU copies). GPU-to-GPU copies are allowed.
- Overlay testing in Deadlock happens in the Hideout or bot matches. In real matches only the
  collector runs: the in-game module is injected with input hooks off, with no Vulkan layer and no
  Electron host (owner has accepted the VAC risk of read-only collection).
- Reads inside the game go through `MemRead` only. The in-game implementation is
  `ReadProcessMemory(GetCurrentProcess(), …)` and requires the full byte count, so an unmapped
  address returns an error and never faults. No raw pointer dereference of game memory anywhere.
- Pointers are plausible only in `0x1000..=0x7fff_ffff_ffff`. Counts are bounded before reading
  (teams ≤ 32, controllers per team ≤ 64, upgrades ≤ 64, name ≤ 128 bytes).
- The collector never calls game functions and never writes game memory. Its only hook is
  `QueryPerformanceCounter` (OS timing API), through MinHook.
- No offsets are pinned to a file hash. Roots come from masked byte patterns; fields come from
  class and field names in the game's schema data.

## Crates

| Path | What it is | Spike |
|---|---|---|
| `common/` | target-process guard, frame-time stats, `uw-spike-bench` | all |
| `vk-layer/` | Vulkan implicit layer `VK_LAYER_UNDERWOLF_spike` | 2, 1 |
| `ipc/` | socket protocol between the Electron host, the layer, the in-game module and `uw-gep` | all |
| `osr-bridge/` | napi-rs addon used by the Electron host | 2, 1 |
| `osr-host/` | Electron offscreen-rendering host (not a Cargo crate) | 2, 1 |
| `dmabuf-consumer/` | standalone dmabuf import check | 2 |
| `input/` | in-game module `uw_spike_input.dll`: edit-mode input hooks, plugin loader | 1, 3 |
| `inject/` | Windows injector and the Linux launcher (`GameSession`): injects the module into the running game | 1, 3 |
| `gep-channel/` | the collector's data channel: named shared memory with info store and event ring | 3 |
| `dl-collector/` | the Deadlock collector plugin `uw_spike_dlcollector.dll` (read-only, main-loop passes) | 3 |
| `gep-helper/` | `uw-gep-helper.exe`: Wine-side reader that relays the channel over TCP | 3 |
| `gep-host/` | `uw-gep`: Linux client (`watch`, `verify`), event provider, feature-status table | 3 |

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

## Electron host and import check (`osr-host/`, `osr-bridge/`, `dmabuf/`, `dmabuf-consumer/`)

```sh
(cd spikes/osr-host && npm install)
cargo run -p uw-spike-consumer &                                   # stands in for the layer
(cd spikes/osr-host && npm start -- --consumer --widget solid)     # expects the consumer to print ff0080ff
```

- Widgets render offscreen to CPU bitmaps, which go to the layer through shared-memory slots
  (plan Amendment A). Electron's GPU shared textures are empty on the NVIDIA driver
  (`docs/spikes/02-shared-texture.md`). The dmabuf path (`sendFrame`, `uw-spike-dmabuf`) is
  kept for AMD and Windows.
- `cargo test -p uw-spike-dmabuf --test gpu_roundtrip` proves dmabuf import on the local GPU
  without Electron.

## Spike 3: Deadlock collector (`dl-collector/`, `gep-helper/`, `gep-host/`)

Data flow: the in-game module (`uw_spike_input.dll`, input hooks off) loads the collector plugin;
the collector writes the channel (named shared memory) from the game's main-loop thread;
`uw-gep-helper.exe` runs in the game's Wine session and relays it over TCP 48611 to `uw-gep`,
which writes JSONL. `uw-gep` and the helper can be restarted while the game runs.

### Install

```sh
./spikes/inject/install.sh
```

Builds and installs into `~/.local/share/underwolf-spike/`: the module, the injector, the
collector (`uw_spike_dlcollector.dll`), `uw-gep-helper.exe`, `uw-spike-inject` and `uw-gep`. It
also reinstalls the Vulkan layer. Needs the `x86_64-pc-windows-gnu` Rust target and mingw
(`sudo pacman -S mingw-w64-gcc`). Run it with the game closed.

### Hideout or bot match, everything on

1. Start the host, which injects the module when the game's layer connects and has it load the
   collector (channel 1, empty start args):

   ```sh
   (cd spikes/osr-host && npm start -- --capture win32 --gep "")
   ```

2. Start Deadlock, go to the Hideout (or a bot match). In another terminal:

   ```sh
   ~/.local/share/underwolf-spike/uw-gep watch --no-inject --jsonl /tmp/hideout.jsonl
   ```

   `--no-inject` because the host already injected the module. `uw-gep` starts the helper in the
   game's Wine session, prints each info update and event as it arrives, and writes one JSONL line
   per emission. Ctrl+C prints the feature-status table (`supported`, `unavailable`,
   `disabled: <reason>`, `error: <reason>` per spec §6.4 key and event).

JSONL lines (`t_ms` is Unix ms at `uw-gep`; `ts` is the collector's clock):

```json
{"t_ms":…,"kind":"info","feature":"match_info","category":"match_info","key":"roster_3","value":"{…}","ts":…}
{"t_ms":…,"kind":"event","name":"kill","data":"{\"kills\":4}","ts":…}
{"t_ms":…,"kind":"status","status":"connected"|"disconnected"|"lost"|"plugin_loaded",…}
```

### Collector only (real matches)

No Vulkan layer, no Electron host, input hooks off. Add `UW_SPIKE_LAYER_DISABLE=1 %command%` to
Deadlock's Steam launch options so the layer doesn't load, start the game, then:

```sh
~/.local/share/underwolf-spike/uw-gep watch --jsonl ~/uw-match-1.jsonl
```

Without `--no-inject`, `uw-gep` injects the module itself (with `ModuleConfig { input_hooks: false }`)
and tells it to load the collector. Leave it running through the match, wait for the post-game
screen, then Ctrl+C. Never run this together with `osr-host --gep`.

### Start arguments (`--args`)

Space-separated `key=value`, passed to the collector at load:

- `discover=<seconds>` (1–600): discovery mode. The collector records which game call sites call
  `QueryPerformanceCounter` and publishes the most frequent as `gep_internal.discovery`
  (status `discovering`, then `discovered`); no passes run. Use it to check the main-loop caller after a patch:
  `uw-gep watch --jsonl /tmp/discover.jsonl --args discover=60`, in the Hideout.
- `corrupt=<Class>.<field>`: moves one resolved schema field offset on purpose, to test the
  fail-safe. Expected: the affected task is disabled (`gep_internal.disabled_tasks`, `invalid: …`)
  or publishes no wrong value. Any wrong published value is a Spike 3 failure. Examples:
  `--args corrupt=PlayerDataGlobal_t.m_iPlayerKills`, then `--args corrupt=C_BaseEntity.m_iTeamNum`.
- `selftest=1`: the collector publishes status `selftest` and one `selftest` event and reads
  nothing from the game.

### Verify against deadlock-api.com

A few minutes after a match ends, deadlock-api.com publishes its metadata. Find the match id in the
log and compare:

```sh
grep '"key":"match_id"' ~/uw-match-1.jsonl | tail -1
~/.local/share/underwolf-spike/uw-gep verify --jsonl ~/uw-match-1.jsonl --match-id <id>
```

`verify` fetches `https://api.deadlock-api.com/v1/matches/<id>/metadata` with `curl` (or reads
`--metadata file.json`) and prints `OK` / `MISMATCH` / `MISSING` per field: the match id and, per
player (matched by account id = low 32 bits of the SteamID64), hero, team, kills, deaths and
assists from each player's last published roster row. Teams are compared through the one-to-one
game-team → API-team mapping most players agree with (printed under the table), because the game
numbers teams 2 and 3 and the API 0 and 1. Exit code: 0 all OK, 1 any `MISMATCH`, 3 only
`MISSING`, 2 usage or fetch error.

### Tests

```sh
cargo test -p uw-spike-gep-host        # provider, status table, verify
cargo test --workspace
cargo test -p uw-spike-dlcollector --test installed_build -- --ignored   # reads the installed client.dll, read-only
```
