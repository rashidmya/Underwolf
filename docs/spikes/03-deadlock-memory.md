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

Setup: runs on 2026-10-21 and 2026-10-22, Deadlock ClientVersion 6807 (Steps 1, 2, 4, 5 first run, 6) and 6810
(Steps 3, 5 rerun, 8; patched mid-window, see Step 7), Proton 11.0, Linux 7.2.11-1-cachyos,
Intel Core i9-13900K.
JSONL logs and watch logs are kept in `~/.local/share/underwolf-spike/runs/c6-*` (not committed:
`*.jsonl` is ignored). Bare file names below are in that directory.

- [x] **Step 1: Hideout and bot match, everything on.** `osr-host --capture win32 --gep ""` +
  `uw-gep watch --no-inject`. Expected: `plugin_status="scanning"`, phase, roster (12 in the bot
  match, one local), items and clock arrive; Ctrl+Tab edit mode still works. Bot match checked by
  hand against the end-of-match scoreboard.
  - Result: **PASS.**
    - Hideout (6807, `c6-s1-hideout.jsonl`, `c6-s1-watch.log`, `c6-s1-host.log`): layer first, then
      `osr-host` (layer retries every 1 s); module and collector loaded; `plugin_status=scanning`
      on the real main-loop path; 20 Hz passes, median 0.045-0.047 ms, max 0.17 ms; phase
      `GameInProgress`, `steam_id`, `game_paused`, roster (3: local + 2 bots), items (3 after the
      owner bought), `match_clock` 1/s. Ctrl+Tab edit mode works.
    - Bot match (Standard Match (Lobby) 121745243, 23:23), checked against the owner's scoreboard
      screenshot: roster snapshot just before End = 12 players, one local (hero 13 = Haze, team 3);
      all 12 K/D/A equal the scoreboard; local items 7 = 7; last `match_clock` 23:23 = scoreboard;
      `match_id` equal; `match_start` at `GameInProgress`, `match_end` at `End` (10 s after
      `PostGame`); 31 kill / 6 death / 9 assist events = local 31/6/9 (+2 kills in the sandbox
      before). Phases Init, MatchIntro, WaitForMapToLoad, PreGameWait, GameInProgress, PostGame,
      End. Pass time median <= 0.114 ms, max 1.72 ms over 199 stats samples; no disabled tasks;
      overlay widget visible throughout.
    - Notes: bots are `Bot0`..`Bot10` with `steam_id` 0; team ids 2 = Hidden King, 3 = Archmother;
      `match_clock` is cleared at `PostGame`; `game_paused` never true there (tested in Step 8).
    - No `match_id`/`match_start` in the Hideout/sandbox (no match there); `game_mode` had no task yet at Step 1 (see Step 8). Overwolf's
      handler generates a local match id in that case; ours does not (no invented data). Owner
      decision pending.
- [x] **Step 2: Cost.** `uw-spike-bench compare`, Hideout, collector off vs on (module injected,
  input hooks off both times). Expected: `PASS` (avg FPS loss ≤ 3.0% and p99 increase ≤ 3.0%).
  - Deviations (owner ruling 2026-10-21): one game launch, Hideout, layer in passthrough every
    run, VSync/cap off. A = 3 x 60 s with nothing injected, then `uw-gep watch` injects module
    (input hooks off) + collector, B = 3 x 60 s. Not alternated, and A has no module injected (the
    collector cannot be unloaded, no on/off switch was built). Drift between A and B would show up
    as collector cost (conservative). Collector load took ~2 s (segment 4, excluded).
  - Avg FPS loss: +0.38% (A 460.28 fps, B 458.55 fps; per run A 457.2/463.7/460.0, B
    458.5/459.4/457.8). The spread within A is larger than the A/B delta: the cost is below the
    bench resolution.
  - p99 frame-time increase: +0.14% (A 3.242 ms, B 3.247 ms)
  - `PASS` / `FAIL`: **PASS** (`~/.local/share/underwolf-spike/runs/c6-s2-compare.txt`, also
    `c6-s2-bench.jsonl`, `c6-s2-bench.log`, `c6-s2-watch.log`)
  - `gep_internal.pass_stats` median / max pass time: median <= 0.044 ms, max 0.202 ms, ~195
    passes per 10 s during B.
  - Script bug: the bench script's own compare read the newest CSV 1 s after the control write,
    before the layer opened it, so it used the wrong files and printed a bogus FAIL. The numbers
    above were recomputed by hand from file mtimes and line counts.
- [x] **Step 3: Accuracy, collector only (owner decision).** At least 3 real matches with
  `uw-gep watch` (no Vulkan layer, no Electron host, input hooks off), then `uw-gep verify
  --team-map <game>=<api>,…`. Confirm the team map by hand in the first match (README, "Verify
  against deadlock-api.com") and use it for all of them. Expected: no `MISMATCH`, no `UNCONFIRMED`.
  - Deviation (owner ruling 2026-10-22): stopped at **1 real match** (plan said at least 3); the
    owner did not want to play more now. Evidence: 24/24 player rows exact (bot match 121745243 by
    scoreboard + real match 121819616 by scoreboard and API); `match_id`, clock, start and end
    correct in both. Rarer cases (abandons, reconnects, hero swaps, surrenders) were not observed;
    more matches can be logged opportunistically.
  - Team map confirmed by hand: the owner played Archmother (screenshot
    `Screenshot_20261022_004549.png`), which is game team 3 = API team 1; Hidden King = game 2 =
    API 0 (same sides as in the bot match). The inferred map game 2 = api 0, game 3 = api 1 held
    for all 12 rows, but `verify` was not rerun with `--team-map` (3 API requests/hour), so the
    team rows stay UNCONFIRMED in the tool's count.

  | Match id | Log | Team map used | OK | MISMATCH | MISSING | UNCONFIRMED | Notes |
  |---|---|---|---|---|---|---|---|
  | 121819616 | `c6-s3-real-6810.jsonl`, `c6-s3-verify.log` | none passed (confirmed by hand: 2 = 0, 3 = 1) | 49 | 0 | 0 | 12 (team rows only) | Standard match, 6810, 46:58, owner won as Archmother. All 12 K/D/A equal the scoreboard; items 11-12 each; last `match_clock` 46:57 vs scoreboard 46:58; `match_start`/`match_end` correct; `disabled_tasks` []. API metadata was 503 (not published) and then 429 (3 requests/hour) until the verify at ~15:18 (log mtime) on 2026-10-22. |

- [x] **Step 4: Latency.** Bot match, screen recording next to `watch`; for 5 kills compare the
  kill-feed time with the `kill` event's `t_ms`. Expected: ≤ 1 s each.
  - **PASS by the owner's eye** (owner ruling 2026-10-21), no screen recording and no per-kill
    table. The owner watched `tail -f c6-s2-bench.jsonl | grep kill` beside the game: kill lines
    appeared "instantly" when the kill landed (3 kills / 1 death in the log at check time).
  - Supporting numbers from the log: relay extra delay (`t_ms` - `ts` - min) over 3078 events:
    median 7 ms, p95 14 ms, max 15 ms; plus a pass interval of at most 50 ms, so under 0.1 s on the
    collector side.

- [x] **Step 5: Fail-safe (Review Focus 1).** `--args corrupt=PlayerDataGlobal_t.m_iPlayerKills`,
  then `--args corrupt=C_BaseEntity.m_iTeamNum`. Expected: the roster task is disabled
  (`invalid: …`) or publishes no wrong value, compared against Step 1. Any wrong value published
  is a Spike 3 failure: stop and report.
  - Result: **first live run FAILED, fixed, rerun PASS.**
  - First run (2026-10-21 23:37, build 6807, collector-only, sandbox with 3 bots), case 1
    `corrupt=PlayerDataGlobal_t.m_iPlayerKills` (delta +8): the offset moved onto
    `m_iPlayerAssists` (+88 to +96). The owner killed a bot with no assist: Bot6 deaths 0 to 1 was
    published correctly, but the local kills stayed 0 (a wrong value published), no kill event,
    `disabled_tasks` [], status `scanning`. Cause: the roster read K/D/A as plain u32 with no
    semantic or layout check, and a shift onto a same-type neighbour is invisible to the pointer,
    count, string and flag validation. Stopped and reported; case 2 not run. Log
    `c6-s5-kills.jsonl`.
  - Fix (owner ruling, Task C6L, commits f528948..b4df974): per required field a relation (anchor
    field, delta) taken from the 6801 dump, checked in `Collector::start` after `corrupt=`. A
    failure disables only the dependent tasks with `invalid: layout: Class.field`. It catches
    same-type neighbour shifts and real reorders. Cost accepted by the owner: a legal Valve
    reorder switches a task off until the table is updated.
  - Rerun on b4df974 (6810, 2026-10-22 00:53-00:57, collector-only, Hideout sandbox):
    - `corrupt=PlayerDataGlobal_t.m_iPlayerKills`: **PASS.** `disabled_tasks` =
      `[{"task":"roster","reason":"invalid: layout: PlayerDataGlobal_t.m_iPlayerKills"}]`; no
      `roster_*`/`items_*` key ever non-null; `steam_id` (from the roster's local player) not
      emitted; phase, `match_clock`, `game_paused` and `plugin_status` unaffected. Log
      `c6-s5r-kills.jsonl`.
    - `corrupt=C_BaseEntity.m_iTeamNum`: **PASS.** Same shape with `invalid: layout:
      C_BaseEntity.m_iTeamNum`; no roster/items key ever non-null; other tasks unaffected. Log
      `c6-s5r-team.jsonl`.
    - Clean check (no `corrupt`): `disabled_tasks` [], roster 3 + items 3 + `steam_id` + phase +
      clock back. Log `c6-s5r-clean.jsonl`.
- [x] **Step 6: Restarts (Review Focus 3).** In a Hideout session, kill and restart `uw-gep` and
  the helper. Expected: the full snapshot arrives again, the game keeps running.
  - Result: **PASS** (Hideout, same game session as the bot match, build 6807):
    1. `uw-gep` SIGINT, then restart.
    2. Helper `kill -9`: `uw-gep` restarts it.
    3. `uw-gep` `kill -9`: the helper is orphaned; the next `uw-gep` stops it and starts a new one.
    - Each time all 33 keys were back within ~1 s, cleared keys were null, and the game kept
      running. The owner closed the game afterwards (22:22), not a crash.
    - Finding: on every reattach the helper reports `lost` (1949, 1979, 2005 = ring sequence
      since collector start) and replays the ring's last ~30 events (`match_clock` ~30 s old). This is
      the known C5 minor: a production consumer should start at the write head or mark replays.
    - Logs `c6-s6-restart.jsonl`, `c6-s6-restart2.jsonl`, `c6-s6-watch.log`, `c6-s6-watch2.log`.
- [x] **Step 7: Robustness across updates.** After the next Deadlock patch, rerun the ignored
  `installed_build` test and Step 1 with **no code changes**. Record which patterns and fields
  survived and the diff of the two schema dumps. If no patch lands within the spike window, record
  that.
  - New build: two patches landed in the spike window, 6801 to 6807 and 6807 to 6810.
    - 6801 to 6807 (client.dll SHA-256 `a7d0d9e8…589e` to `96ee2a33…`, installed 2026-10-21
      19:57). `tier0`/`engine2` unchanged by it.
    - 6807 to 6810 (`96ee2a33…` to `5c50c21e…`, 23:48). `tier0`/`engine2` still unchanged.
  - Patterns / fields that survived, with no code changes: on 6807 `installed_build` passes 3/3
    (all client.dll patterns, schema fields and the main-loop callers resolve); the code moved
    (GAME_TIME hit RVA 0x1975d70 to 0x1975d00). On 6810 the collector loaded from the unchanged
    installed DLL (commit f528948): `plugin_status` `scanning`, `disabled_tasks` [], phase, clock,
    roster and items sane in the Hideout. Later builds (b4df974, 1472875) also pass
    `installed_build` 3/3 on 6810 (layout relations hold). `installed_build` was not run on 6810
    with f528948 itself; the later builds' changes were the layout check and the Step 8 tasks, not
    patch fixes.
  - Schema dump diff: 6801 vs 6807 empty (3963 classes, 13832 fields, 0 added/removed/moved);
    6801 vs 6810 empty (same counts). Dumps `schema-6807.json` and `schema-6810.json` are in the session scratchpad (not committed).
  - Step 1 rerun: Steps 1, 2, 4 and 6 all ran on 6807, and Steps 3, 5 rerun and 8 on 6810, with
    no code changes caused by the patches.
  - Caveat: `tier0`/`engine2` were not patched, so the main-loop patterns have not been through a
    patch yet.
- [ ] **Step 8: Timeboxed features (2 days, owner decision).** `game_mode`, `game_paused` (if not
  already from the clock task), `team_score`, `match_outcome`, `tower_destroyed`,
  `incoming_damage`: candidates from `schema-6801.json`, a task each with a test in
  `tests/tasks.rs`, checked live. Anything not found or not matching the game's display is
  recorded with its alternative source (console log, local files, deadlock-api.com) or as
  `unavailable`. Never derive or estimate a value.

  | Feature | Candidate class.field | Matches the game's display? | Outcome |
  |---|---|---|---|
  | `game_mode` | `C_CitadelGameRules.m_eMatchMode` (0xa8, enum `ECitadelMatchMode`) + `m_eGameMode` (0xac, enum `ECitadelGameMode`); enum names paired by name from the game's own schema enum records | Yes: private lobby gave `{"match_mode":"PrivateLobby","game_mode":"Normal"}`; cleared at match end; not published in the Hideout (both enums unnamed there) | PASS (source: game memory) |
  | `game_paused` | `C_GameRules.m_bGamePaused` (0x38, u8 bool; already read by the clock) | Yes: 2 pauses gave exactly true, false, true, false; clock frozen during both (10 s, 5 s). Venue: private lobby, owner host | PASS (source: game memory) |
  | `team_score` | `C_Team.m_iScore` (0x620) per team (`C_BaseEntity.m_iTeamNum` selects Amber/Sapphire); the game's top bar shows it as the team's kills | Yes: mid-match HUD orange 7 / blue 5 = log amber 7 / sapphire 5; final 40/63 = scoreboard; all 12 K/D/A identical. In the Hideout: 0-0, then amber 1 = roster sum after an owner kill | PASS (source: game memory) |
  | `match_outcome` | `C_CitadelGameRules.m_iWinningTeam` (0xc0), published only in PostGame and End | Yes: `{"winning_team":"SAPPHIRE"}` at PostGame, scoreboard "Archmother Victorious"; null when the phase left PostGame for None (no End phase was logged in this match; `match_end` fired at that transition, 10.2 s after PostGame). Value is the winning team (AMBER/SAPPHIRE, as Overwolf documents), not local victory/defeat | PASS (source: game memory) |
  | `tower_destroyed` | `C_CitadelTeam.m_nBossesAlive` (0x6b8): the event fires when a team's count drops, data = the team that lost it ("Amber"/"Sapphire"); gated to GameInProgress and Normal mode | Partly: 22 events (Amber 14, Sapphire 8) from 08:11 to 22:55; the owner-observed subset matched (2 enemy Guardians gave 2x Amber, own Guardian gave Sapphire, shrine). No event in PostGame (the Patron kill) by design. The full 22-event count is **not yet checked** against an objective list (deadlock-api metadata gave 503 "Steam is busy" at 15:17); pending | Implemented (source: game memory); accuracy **pending**, not claimed PASS |
  | `incoming_damage` | none usable: only via the Panorama HUD object `CCitadel_Hud_DamageReport` (non-schema offsets) or a message hook, outside the collector rules (schema fields only; QPC is the only hook) | n/a | `unavailable` (no task; not emitted) |

  - Live check: private lobby with bots, match 122217736, owner host, team 3, build 1472875 on
    6810, log `c6-s8-live.jsonl` (watch log `c6-s8-live-watch.log`). In the Hideout: no
    `tower_destroyed`, `disabled_tasks` [].
  - Final `match_clock` 26:24 = scoreboard 26:24 (the missing last second of Step 3 did not
    reproduce).
  - Leave-mid-match (match 122227637, private lobby): about 3 min of `GameInProgress`, 4 kills,
    then the owner left. Phase went to None and `match_end` fired (no `PostGame`);
    `game_mode`, `match_id` and `team_score` were cleared; `match_outcome` was never set; no
    `tower_destroyed` during or after, nor in the Hideout afterwards. PASS. Note: `match_end` also
    fires on leaving (the phase leaves the match), not only on a finished match.
  - Event data shapes stay raw in the collector (owner ruling); the provider layer normalizes
    (for example `match_clock` as "01:00", null data for kill/death/assist/match_start/match_end,
    `tower_destroyed` team string). Provider requirement for C7.

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
