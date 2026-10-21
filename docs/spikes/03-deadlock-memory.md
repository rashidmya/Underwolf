# Spike 3: Deadlock live data via read-only memory reading

## Question

> Prove the §6.4 features can be read from inside the game process (by the injected module) on
> Linux (Proton) and Windows, and how robust the approach is across game updates. Output: a
> feature-by-source table, noting what comes from memory vs. other sources.

(Spec §8.3. Linux only for now per plan Amendment C; Windows is not scheduled.)

## Verdict

`PENDING`

The code is built (Tasks C1–C5). The live runs below (Task C6) have not been done yet. Fill in
the results, then the verdict and the sections after the table (Task C7).

## Evidence

### Evidence before planning (2026-10-21, offline, read-only)

Deadlock updated to ClientVersion 6801 (`client.dll` SHA-256 `a7d0d9e8…589e`). underlock's
`inspectDeadlockModule` still resolved every byte pattern and schema field on it: **all patterns
and schema fields survived** (field offsets unchanged, RVAs moved). The clock was lost only because
its roots were pinned to the old file's hash. So the clock's roots became byte patterns (Task C3).
The schema dump of this build is `spikes/dl-collector/dumps/schema-6801.json`.

### Live runs (Task C6)

Setup: run on <date>, Deadlock build <ClientVersion>, Proton <version>, <machine>.
JSONL logs are kept as <paths> (not committed: `*.jsonl` is ignored).

- [ ] **Step 1: Hideout and bot match, everything on.** `osr-host --capture win32 --gep ""` +
  `uw-gep watch --no-inject`. Expected: `plugin_status="scanning"`, phase, roster (12 in the bot
  match, one local), items and clock arrive; Ctrl+Tab edit mode still works. Bot match checked by
  hand against the end-of-match scoreboard.
  - Result:
- [ ] **Step 2: Cost.** `uw-spike-bench compare`, Hideout, collector off vs on (module injected,
  input hooks off both times). Expected: `PASS` (avg FPS loss ≤ 3.0% and p99 increase ≤ 3.0%).
  - Avg FPS loss: 
  - p99 frame-time increase: 
  - `PASS` / `FAIL`: 
  - `gep_internal.pass_stats` median / max pass time: 
- [ ] **Step 3: Accuracy, collector only (owner decision).** At least 3 real matches with
  `uw-gep watch` (no Vulkan layer, no Electron host, input hooks off), then `uw-gep verify`.
  Expected: no `MISMATCH`.

  | Match id | Log | OK | MISMATCH | MISSING | Notes |
  |---|---|---|---|---|---|
  |  |  |  |  |  |  |
  |  |  |  |  |  |  |
  |  |  |  |  |  |  |

- [ ] **Step 4: Latency.** Bot match, screen recording next to `watch`; for 5 kills compare the
  kill-feed time with the `kill` event's `t_ms`. Expected: ≤ 1 s each.

  | Kill | Kill feed (video time) | `kill` `t_ms` | Delta |
  |---|---|---|---|
  | 1 |  |  |  |
  | 2 |  |  |  |
  | 3 |  |  |  |
  | 4 |  |  |  |
  | 5 |  |  |  |

- [ ] **Step 5: Fail-safe (Review Focus 1).** `--args corrupt=PlayerDataGlobal_t.m_iPlayerKills`,
  then `--args corrupt=C_BaseEntity.m_iTeamNum`. Expected: the roster task is disabled
  (`invalid: …`) or publishes no wrong value, compared against Step 1. Any wrong value published
  is a Spike 3 failure: stop and report.
  - `corrupt=PlayerDataGlobal_t.m_iPlayerKills`:
  - `corrupt=C_BaseEntity.m_iTeamNum`:
- [ ] **Step 6: Restarts (Review Focus 3).** In a Hideout session, kill and restart `uw-gep` and
  the helper. Expected: the full snapshot arrives again, the game keeps running.
  - Result:
- [ ] **Step 7: Robustness across updates.** After the next Deadlock patch, rerun the ignored
  `installed_build` test and Step 1 with **no code changes**. Record which patterns and fields
  survived and the diff of the two schema dumps. If no patch lands within the spike window, record
  that.
  - New build:
  - Patterns / fields that survived:
  - Schema dump diff:
  - Step 1 rerun:
- [ ] **Step 8: Timeboxed features (2 days, owner decision).** `game_mode`, `game_paused` (if not
  already from the clock task), `team_score`, `match_outcome`, `tower_destroyed`,
  `incoming_damage`: candidates from `schema-6801.json`, a task each with a test in
  `tests/tasks.rs`, checked live. Anything not found or not matching the game's display is
  recorded with its alternative source (console log, local files, deadlock-api.com) or as
  `unavailable`. Never derive or estimate a value.

  | Feature | Candidate class.field | Matches the game's display? | Outcome |
  |---|---|---|---|
  | `game_mode` |  |  |  |
  | `game_paused` |  |  |  |
  | `team_score` |  |  |  |
  | `match_outcome` |  |  |  |
  | `tower_destroyed` |  |  |  |
  | `incoming_damage` |  |  |  |

### Feature-by-source table

One row per spec §6.4 feature and event. Source: memory / console log / deadlock-api.com /
unavailable.

| Feature/key | Source | Resolution (schema name / pattern) | Accuracy result | Survived patch | Notes |
|---|---|---|---|---|---|
| `gep_internal.version_info` |  |  |  |  |  |
| `game_info.phase` |  |  |  |  |  |
| `game_info.game_mode` |  |  |  |  |  |
| `game_info.match_history` |  |  |  |  |  |
| `game_info.steam_id` |  |  |  |  |  |
| `match_info.team_score` |  |  |  |  |  |
| `match_info.match_id` |  |  |  |  |  |
| `match_info.roster` |  |  |  |  |  |
| `match_info.items` |  |  |  |  |  |
| `match_info.incoming_damage` |  |  |  |  |  |
| `match_info.match_outcome` |  |  |  |  |  |
| event `match_start` |  |  |  |  |  |
| event `match_end` |  |  |  |  |  |
| event `kill` |  |  |  |  |  |
| event `death` |  |  |  |  |  |
| event `assist` |  |  |  |  |  |
| event `match_clock` |  |  |  |  |  |
| event `game_paused` |  |  |  |  |  |
| event `tower_destroyed` |  |  |  |  |  |

### Linux differences used, and the main-loop caller

To fill in (Task C7): the module's timer calls the plugin's `present` every 16 ms; the Wine-side
helper relays the channel; `ReadProcessMemory` on self for safe reads; the chosen main-loop
caller of `QueryPerformanceCounter`.

## Mechanism chosen

## Risks

## Not tested and why

## Decisions needed from owner
