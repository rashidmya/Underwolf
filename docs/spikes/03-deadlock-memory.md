# Spike 3: Deadlock live data via read-only memory reading

## Question

> Prove the §6.4 features can be read from inside the game process (by the injected module) on
> Linux (Proton) and Windows, and how robust the approach is across game updates. Output: a
> feature-by-source table, noting what comes from memory vs. other sources.

(Spec §8.3. Linux only for now per plan Amendment C; Windows is not scheduled.)

## Verdict

**Linux: GO, with owner-accepted deviations.** Windows is not scheduled (Amendment C).

- The collector runs inside Deadlock (Proton 11.0) as a plugin of the Spike 1 in-game module, on
  the game's main-loop thread, and only reads its own process's memory. Its data reaches the
  Linux client through Overwolf-style shared memory and a Wine-side helper.
- Verified live: `phase`, `match_id`, `steam_id`, `roster`, `items` (local count, bot match
  only), `match_clock`, `match_start`/`match_end` and `kill`/`death`/`assist` (bot match plus one
  real match: 24/24 player rows exact; API verify with `--team-map 2=0,3=1`: 61 OK, 0 MISMATCH,
  0 MISSING, 0 UNCONFIRMED). From the timebox: `game_mode`, the `game_paused` event,
  `team_score` and `match_outcome`.
- `tower_destroyed` is implemented; the owner-observed subset matched, but the full count is
  unverified. `incoming_damage` and `match_history` are unavailable.
- Cost: +0.38% avg FPS, +0.14% p99 (PASS). Two client.dll patches (6801 to 6807 to 6810) needed
  no code change. The main-loop patterns (tier0/engine2) have not been through a patch.
- Deviations (owner rulings): 1 real match instead of at least 3; latency checked by eye; the
  bench was not alternated and had nothing injected in A.
- Fail-safe: the first Step 5 run published a wrong value (local kills stayed 0) under
  deliberate corruption. It was stopped and reported; the owner ruled a layout check (Task C6L),
  and the rerun switched the roster off instead (PASS).
- Gate (Task C7 Step 3): **passes.** Phase, roster, kill/death/assist and match_clock were read
  live, and the roster and kill/death/assist events matched the scoreboard (the roster also the
  API). The only wrong value was the Step 5 case above, already reported and fixed; the checks
  made found no other.
- Disclosure: Task C6-8c read the raw decompile of Overwolf's plugin
  (`~/Dev/underwolf-research/evidence/gep_deadlock.dll.evidence.json`), beyond Amendment C's
  input rule at the time, to learn how Overwolf produces `tower_destroyed` and `incoming_damage`.
  The owner later allowed it (ruling 2026-10-22). The implementer ran no reverse engineering of
  Overwolf and copied no Overwolf code; the field used is a plain schema field.

## Evidence

### Evidence before planning (2026-10-21, offline, read-only)

Deadlock updated to ClientVersion 6801 (`client.dll` SHA-256 `a7d0d9e8…589e`). underlock's
`inspectDeadlockModule` still resolved every byte pattern and schema field on it: **all patterns
and schema fields survived** (field offsets unchanged, RVAs moved). The clock was lost only because
its roots were pinned to the old file's hash. So the clock's roots became byte patterns (Task C3).
The schema dump of this build is `spikes/dl-collector/dumps/schema-6801.json`.

### Live runs (Task C6)

Setup: runs on 2026-10-21 and 2026-10-22, Deadlock ClientVersion 6807 (Steps 1, 2, 4, 5 first
run, 6) and 6810 (Steps 3, 5 rerun, 8; patched mid-window, see Step 7), Proton 11.0, Linux
7.2.11-1-cachyos, Intel Core i9-13900K.
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
    - No `match_id`/`match_start` in the Hideout/sandbox (no match there); `game_mode` had no
      task yet at Step 1 (see Step 8). Overwolf's handler generates a local match id in that
      case; ours does not (no invented data). Owner decision pending.
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
    `~/Pictures/Screenshots/Screenshot_20261022_004549.png`), which is game team 3 = API team 1;
    Hidden King = game 2 = API 0 (same sides as in the bot match). The inferred map game 2 = api
    0, game 3 = api 1 held for all 12 rows. `verify` was then rerun with `--team-map 2=0,3=1`
    (2026-10-22 ~16:57, 1 request): 61 OK, 0 MISMATCH, 0 MISSING, 0 UNCONFIRMED (the 12 team rows
    OK). Log `c6-s3-verify-teammap.log`.

  | Match id | Log | Team map used | OK | MISMATCH | MISSING | UNCONFIRMED | Notes |
  |---|---|---|---|---|---|---|---|
  | 121819616 | `c6-s3-real-6810.jsonl`, `c6-s3-verify.log` | none passed (confirmed by hand: 2 = 0, 3 = 1) | 49 | 0 | 0 | 12 (team rows only) | Standard match, 6810, 46:58, owner won as Archmother. All 12 K/D/A equal the scoreboard; items 11-12 each; last `match_clock` 46:57 vs scoreboard 46:58; `match_start`/`match_end` correct; `disabled_tasks` []. API metadata was 503 (not published) and then 429 (3 requests/hour) until the verify at ~15:18 (log mtime) on 2026-10-22. |
  | 121819616 | `c6-s3-real-6810.jsonl`, `c6-s3-verify-teammap.log` | `2=0,3=1` | 61 | 0 | 0 | 0 | Rerun with the confirmed team map (2026-10-22 ~16:57); the 12 team rows are OK. |

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
      since collector start) and replays the ring's last ~30 events (`match_clock` ~30 s old).
      This is the known C5 minor: a production consumer should start at the write head or mark
      replays.
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
    6801 vs 6810 empty (same counts). Dumps `schema-6807.json` and `schema-6810.json` are in the
    session scratchpad (not committed).
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
  | `tower_destroyed` | `C_CitadelTeam.m_nBossesAlive` (0x6b8): the event fires when a team's count drops, data = the team that lost it ("Amber"/"Sapphire"); gated to GameInProgress and Normal mode | Partly: 22 events (Amber 14, Sapphire 8) from 08:11 to 22:55; the owner-observed subset matched (2 enemy Guardians gave 2x Amber, own Guardian gave Sapphire, Walker (both sides), shrine). No event in PostGame (the Patron kill) by design. The full 22-event count is **unverified** against an objective list: deadlock-api metadata gave 503 "Steam is busy" at 15:17, 16:21 and 16:56 (attempts stopped), and private-lobby matches may never be published | Implemented (source: game memory); full count **unverified**, not claimed PASS |
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
| `gep_internal.version_info` | memory (the collector's own text) | Collector name and version, plus the schema record layout it found in the game (`disk-class-v2`) | Published live at start in every run; the value is the collector's own (the layout name comes from the game's schema data) | Yes (the layout resolved on 6807 and 6810) | `{"collector":"uw-spike-dlcollector 0.0.0","schema_layout":"disk-class-v2"}` |
| `game_info.phase` | memory | `RULES` pattern (game rules global) + `C_CitadelGameRules.m_eGameState` | Read live in every run. Bot match: Init, MatchIntro, WaitForMapToLoad, PreGameWait, GameInProgress, PostGame, End. Private lobby: PostGame to None, no End | Yes | GameInProgress in the Hideout/sandbox. Required field: without it the collector publishes only its error status (`plugin_status`, `plugin_error`) |
| `game_info.game_mode` | memory | `C_CitadelGameRules.m_eMatchMode` + `m_eGameMode`, named by fixed tables copied from the 6810 `ECitadelMatchMode` / `ECitadelGameMode` schema enums (prefix stripped) | PASS: private lobby `{"match_mode":"PrivateLobby","game_mode":"Normal"}`, cleared at match end | Not exercised: added on 6810 (its fields are equal in all three dumps) | Not published in the Hideout (neither value has a name there); unnamed values are left out. Field-to-enum pairing proven offline on 6810 from the field records (`m_eMatchMode` 0x18076c3bd / `ECitadelMatchMode` 0x18076c3cd; `m_eGameMode` 0x18076c3f9 / `ECitadelGameMode` 0x18076c409); `installed_build` pins the enum tables |
| `game_info.match_history` | unavailable | None: no task or provider reads it | n/a | n/a | Not attempted in Spike 3; the status table reports it `unavailable` |
| `game_info.steam_id` | memory | The roster's local row: `CBasePlayerController.m_steamID` where `m_bIsLocalPlayerController` is set; `uw-gep`'s provider emits it when it changes | PASS: published in every run with the roster; the roster's steam ids matched all 12 API players of the real match (`verify` keys rows by them: 0 MISSING, 0 MISMATCH) | Yes | Not emitted while the roster is off (Step 5 rerun). Bots have steam id 0 |
| `match_info.team_score` | memory | `TEAMS` pattern (team list) + `C_Team.m_iScore` per team; `C_BaseEntity.m_iTeamNum` 2 = amber, 3 = sapphire | PASS: mid-match HUD 7 / 5 = log; final 40 / 63 = scoreboard | Not exercised: added on 6810 | The game's top bar shows `m_iScore` as the team's kills (static evidence, 8b). Also published in the Hideout (0-0, then amber 1 after an owner kill). `installed_build` pins the team numbers to the game's parser |
| `match_info.match_id` | memory | `RULES` + `C_CitadelGameRules.m_unMatchID` (0 is not emitted) | PASS: bot match 121745243 and real match 121819616 equal the scoreboard; `verify` OK | Yes | None in the Hideout/sandbox (no match there; see Decisions). Required field |
| `match_info.roster` | memory | `TEAMS` + `ENTITIES` patterns (team list, controller handles, entity list); `C_Team.m_aPlayerControllers`, `C_BaseEntity.m_iTeamNum`, `CBasePlayerController.m_iszPlayerName` / `m_steamID` / `m_bIsLocalPlayerController`, `CCitadelPlayerController.m_PlayerDataGlobal`, `PlayerDataGlobal_t.m_nHeroID` / `m_iPlayerKills` / `m_iDeaths` / `m_iPlayerAssists` | PASS: 24/24 player rows exact (bot match by scoreboard; real match by scoreboard and API: `verify --team-map 2=0,3=1` gave 61 OK, 0 MISMATCH, 0 MISSING, 0 UNCONFIRMED, hero and team rows included) | Yes | Step 5 first run published a wrong value under deliberate corruption (see Verdict); the layout check now switches the roster off instead. Keys `roster_0`..`roster_11`, one slot per player. Bots are `Bot0`..`Bot10` |
| `match_info.items` | memory | The roster's walk + `PlayerDataGlobal_t.m_vecUpgrades` (at most 64) | Count only: local 7 = scoreboard 7 (bot match); real match 11-12 per player (not compared) | Yes | Item ids were not compared with any list (`verify` does not check items). Keys `items_0`..`items_11` |
| `match_info.incoming_damage` | unavailable | None. Overwolf reads the HUD object `CCitadel_Hud_DamageReport` at fixed non-schema offsets (only while the damage panel is open); the HUD's numbers arrive as user messages (`k_EUserMsg_Damage`, `k_EUserMsg_RecentDamageSummary`) | n/a | n/a | Outside the collector rules (schema fields only; QPC is the only hook). No schema field holds incoming damage per source. Post-match only: deadlock-api.com metadata has `player_damage_taken` (a total, no per-source split, no time filter) |
| `match_info.match_outcome` | memory | `RULES` + `C_CitadelGameRules.m_iWinningTeam` (2 = AMBER, 3 = SAPPHIRE), only in PostGame and End, where the game's match-end panel shows the winner | PASS: `{"winning_team":"SAPPHIRE"}` at PostGame, scoreboard "Archmother Victorious" (1 match) | Not exercised: added on 6810 | The winning team, as Overwolf documents it, not local victory/defeat. Null in other phases; never set when leaving mid-match |
| event `match_start` | memory (via `uw-gep`'s provider) | Provider: `phase` reaches GameInProgress with a known `match_id`; once per match id | PASS: at GameInProgress in the bot match and the real match | Yes | Never fires in the Hideout/sandbox (no match id). Data `{"match_id":"…"}` (Overwolf: null) |
| event `match_end` | memory (via provider) | Provider: `phase` leaves GameInProgress, PostGame or PostGame_PlayOfTheGame (or is cleared) after a start | PASS: at End, 10 s after PostGame (bot match, real match); at PostGame to None in the private lobby (no End); on leaving mid-match (no PostGame) | Yes | Fires on leaving too, not only at a finished match. Data `{"match_id":"…"}` (Overwolf: null) |
| event `kill` | memory (via provider) | Provider: the local roster row's `m_iPlayerKills` up by exactly 1 within one match id and steam id | PASS: 31 events = scoreboard 31 (bot match). Latency: "instantly" by the owner's eye; relay delay median 7 ms, p95 14 ms, max 15 ms, plus a pass interval of at most 50 ms | Yes | Data `{"kills":N}` (Overwolf: null) |
| event `death` | memory (via provider) | Same, `m_iDeaths` | PASS: 6 events = scoreboard 6 (bot match) | Yes | Data `{"deaths":N}` (Overwolf: null) |
| event `assist` | memory (via provider) | Same, `m_iPlayerAssists` | PASS: 9 events = scoreboard 9 (bot match) | Yes | Data `{"assists":N}` (Overwolf: null) |
| event `match_clock` | memory | Clock task: `CLOCK_SECONDS`, `GAME_TIME`, `ADJUST_TIME` and `CONVAR_GETTER` patterns (three ConVars, the globals, the time-adjust function, which must equal the rules vtable slot +0x220 on every read) + `C_GameRules.m_nTotalPausedTicks` / `m_nPauseStartTick` / `m_bGamePaused`, `C_CitadelGameRules.m_flGameStartTime` / `m_eGameMode` / `m_eGameState` | PASS: last value = scoreboard in the bot match (23:23) and the private lobby (26:24); real match 46:57 vs 46:58 (the final second was not published before PostGame cleared the clock); frozen during pauses | Yes (`GAME_TIME` moved from RVA 0x1975d70 to 0x1975d00 and still resolved) | The collector's `clock` event, renamed by the provider; once per second, GameInProgress only. Data `{"match_clock":"MM:SS"}` (Overwolf: "01:00"). Also the info key `match_info.match_clock` |
| event `game_paused` | memory | Clock task: a change of `C_GameRules.m_bGamePaused` between two passes in GameInProgress; data `true` / `false` | PASS: 2 pauses gave exactly true, false, true, false (private lobby, owner host) | Not exercised: the true/false event was added on 6810 | The info key `match_info.game_paused` ("true"/"false") is older and survived both patches, as did an older pause-only event (`{"game_paused":true}`, since C3b), which fired on both pauses of the real match (6810, unchanged f528948 build; not compared with the display) |
| event `tower_destroyed` | memory | `C_CitadelTeam.m_nBossesAlive` (checked against `m_nBossesMax`) for teams 2 and 3 of the `TEAMS` list: one event per unit the count drops, data = the team that lost it; only in GameInProgress and Normal mode; baseline per game rules object and match id | Partly: 22 events (Amber 14, Sapphire 8); the owner-observed subset matched; the full count is unverified (Step 8) | Not exercised: added on 6810 | The field Overwolf's plugin compares (learned from its raw decompile, see Verdict). The Patron's loss falls in PostGame and fires no event. Data `"Amber"` / `"Sapphire"` (a JSON string) |

- "Survived patch": the row's patterns and fields resolved on 6807 and 6810 with the code written
  for 6801, and no change (Step 7). Rows added in Step 8 ran only on 6810: "not exercised".
- Every row also depends on the main-loop caller patterns (`tier0.dll`, `engine2.dll`). Neither
  DLL was patched in the window, so that part is not exercised.
- "via provider": `uw-gep`'s provider computes the event from keys read from memory, by the
  exact rules given; nothing is estimated.

### Linux differences used, and the main-loop caller

- **The module's timer calls `present`.** The in-game module calls `uw_plugin_start` once, then
  `uw_plugin_present` about every 16 ms from its own thread. The Windows-side module has no
  present hook, because drawing happens in the native Vulkan layer. `present` flushes keys that
  a busy channel mutex held back and reports `stalled` after 5 s without a pass. The passes
  themselves run only on the game's main thread.
- **The Wine-side helper relays the channel.** The Linux host can't open Wine's named shared
  memory, so `uw-gep-helper.exe` runs in the game's Wine session (started by `uw-gep` with the
  game's Proton and prefix). It opens `Local\uw-gep-<channel>-…`, sends a full snapshot on
  attach, then the changes and events, over TCP 127.0.0.1:48611 to `uw-gep`. After a disconnect
  it attaches again, and `uw-gep` restarts it if it exits (Step 6).
- **`ReadProcessMemory` on itself for safe reads.** Every game read is
  `ReadProcessMemory(GetCurrentProcess(), …)` and must return the full byte count, so an
  unmapped address gives an error, never a fault. Game memory is never dereferenced directly.
- **Main-loop caller of `QueryPerformanceCounter`.** Two return addresses must match:
  - immediate `tier0.dll+0x14222e`, the return site of `call [QueryPerformanceCounter]` in
    tier0's `Plat_FloatTime` export (RVA 0x142210, pattern `MAIN_LOOP_IMMEDIATE`, `ret_offset`
    0x1e);
  - outer `engine2.dll+0x208a73`, the first `Plat_FloatTime` call in each iteration of engine2's
    frame loop, under engine2+0x209d06, engine2+0x20cadf and deadlock.exe+0x4be0 (pattern
    `MAIN_LOOP_OUTER`, `ret_offset` 6).
  - Found with discovery mode (`discover=10`, Hideout, 6807): 1435 chains in 10 s. Hundreds run
    at the frame rate on the window's thread; this one had 392 calls (39.2/s, the frame rate),
    all on that thread, and is the shallowest. The outer frame is needed because 172 chains
    (~18k calls/s) share tier0+0x14222e.
  - Gate order: immediate compare, not already in a pass, 50 ms since the last pass, then the
    outer frame (a stack walk of up to 32 frames). Passes ran at 20 Hz (Step 1; ~195 per 10 s in
    Step 2). Each pattern matches once in its module, with no base relocations inside it.

## Mechanism chosen

The Overwolf design of spec §6.4, under Proton:

- **Loading.** `uw_spike_dlcollector.dll` is a plugin of the Spike 1 in-game module
  (`uw_spike_input.dll`, injected into the running game from outside by remote `LoadLibraryW`,
  with no game-side setup). `uw-gep` answers the module's hello with `ModuleConfig` (input hooks
  off) and `LoadPlugin`. The plugin exports `uw_plugin_start`/`present`/`stop` and refuses to be
  unloaded.
- **Timing.** One MinHook detour on `ntdll!RtlQueryPerformanceCounter` (Wine's
  `kernel32!QueryPerformanceCounter` forwards there), the collector's only hook. A call from any
  other caller costs one compare and a tail call. A pass runs on the game's main thread at most
  every 50 ms and never re-enters. Discovery mode (`discover=`, `discover_out=`) re-derives the
  caller after an update.
- **Roots and fields.** Masked byte patterns for the roots: `RULES`, `CLASSES`, `TEAMS` and
  `ENTITIES` (from underlock), four clock patterns and two main-loop patterns. Fields by class
  and field name from the game's own schema data (layout `disk-class-v2`). Nothing is pinned to
  a file hash.
- **Fail-safe.**
  - A missing required root or field: `plugin_status=error` with a reason, and nothing else.
  - A missing per-task root or field: that task is left out (`unresolved: …`).
  - Layout check: 23 relations (field, nearest neighbour, delta) from the 6801 dump, checked at
    start. A broken one switches off the tasks that read the field
    (`invalid: layout: Class.field`).
  - Each task reads again what it used and drops a torn pass. The runner switches a task off
    after 11 overruns of 20 ms (`slow`), on a panic (`crashed`) or on `Invalid`, and clears its
    keys.
- **Tasks.** `phase`, `match_id`, `clock`, `roster` (with items), `game_mode`, `team_score`,
  `match_outcome` and `tower_destroyed`. One info transaction per pass, with only the changed
  keys.
- **Channel.** Overwolf-style named shared memory per channel id: connect/disconnect events, a
  version + pid block, an info store with a fixed schema (category → key → max length) and
  per-key timestamps, signalled once per transaction, and a 30-record event ring, throttled to 30
  events per 200 ms with a retry queue. The game thread tries the channel mutexes with a 0 ms
  wait only; when the helper holds one, the keys are kept for the next pass.
- **Linux client.** `uw-gep watch` (provider, feature-status table, JSONL log) and `uw-gep verify`
  (per-player check against deadlock-api.com).
- **Provider requirements (owner ruling).** Event data stays raw in the collector. The product's
  provider layer turns it into Overwolf's documented data, as Overwolf's JS handler does over
  its native plugin: `match_clock` as "01:00", null for kill/death/assist/match_start/match_end,
  and the plain team name for `tower_destroyed`. `match_outcome` is the winning team; local
  victory/defeat, if wanted, belongs in the provider.

## Risks

Data and events:
- `match_end` fires whenever the phase leaves the match: at End (bot and real match), at
  PostGame to None in a private lobby (no End phase was logged there), and on leaving mid-match
  (no PostGame). It also fires on a new match id without a phase change, and when a collector
  error clears the phase.
- Hideout/sandbox: no `match_id`, so no `match_start`/`match_end`; `game_mode` is not published
  (neither enum value has a name there); `team_score` is published (Overwolf: "of the current
  match").
- `tower_destroyed` counts boss-class entities leaving a team. That this is the removal at death
  is not proven offline (server vtable slot 14). The server's remove call takes 1 off
  unconditionally, so one event per objective relies on one remove per boss.
- `tower_destroyed` fires nothing for the Patron: its loss falls in PostGame, where the task is
  gated off. The task runs only in Normal mode.
- `match_outcome` is null in PostGame_PlayOfTheGame (as the game's match-end panel shows none)
  and for any winner other than 2 or 3.
- Kill/death/assist fire only on a +1 between two passes: two kills inside one 50 ms pass give no
  event.
- `uw-gep` de-duplicates events on (name, ms timestamp), so two same-name events pushed in the
  same millisecond (two towers lost in one pass) are logged as one. Spike client only.
- A torn or failed read keeps the task's last published value until the next good pass, and a
  persistent read error never switches the task off.
- Overwolf's plugin knows fewer enum names (GameMode Normal..ExploreNYC, MatchMode
  Unranked..Tutorial); ours emits every name the 6810 enums have, such as `HeroLabs` or
  `Internal`. A value added by a later build has no name and is left out.
- The status table marks a row `supported` on its first value (`team_score` already in the
  Hideout): `supported` means seen, not verified.

Updates:
- The main-loop patterns have not been through a patch. The tier0 one depends on
  `Plat_FloatTime`'s tail (a static-initializer twin at tier0+0x5c80 matches up to `cvtsi2sd`);
  the engine2 one keeps register choices. A recompile gives "main-loop caller not found": the
  plugin reports it and runs no passes (safe, but no data).
- The clock patterns are long (85, 106 and 111 bytes) and pin the offsets the clock arithmetic
  uses, so a compiler change switches off only the clock. The `GAME_TIME` paused-branch `je`
  target was not verified offline (the clock froze correctly in the live pauses).
- A legal Valve field reorder switches the affected tasks off until `RELATIONS` and the dump test
  are updated (cost accepted by the owner). `m_nTotalPausedTicks` is anchored to the generated
  `__m_pChainEntity` (delta 40), which may move for unrelated reasons (clock off, the safe side).
- The roster's team-list header re-read has no test (removing it passes every test); the code is
  correct today.

Channel and client:
- Ring replay on reattach: a new helper reports `lost` (the ring sequence since collector start)
  and replays the ring's last ~30 events (`match_clock` ~30 s old). A production consumer must
  start at the write head or mark replays.
- The helper reports a wait failure as a disconnect (possible tight re-attach loop), polls for
  the channel every 250 ms, and `uw-gep` kills every `uw-gep-helper.exe` of the user at start.
- Silent drops: the publish path drops oversize or unknown keys, and the event retry queue drops
  the oldest beyond 256; neither is counted.
- If `InfoWriter::attach` fails, the channel writer drops the pending keys; keys merged after a
  busy mutex share the newest timestamp; the throttle budget is spent when the ring mutex is
  busy.
- The plugin can't be unloaded, and a failed start answers "already loaded" on a retry, so both
  need a game restart.
- `uw-gep verify` attributes rows by the latest match id in the log and silently drops an API
  player without `account_id`.

Cost:
- Stack walk: about 424 calls per frame reach tier0+0x14222e on the window thread (plus about 44
  on other threads), and each takes the plugin lock with `try_lock`. Once the 50 ms interval has
  passed, each walks up to 32 frames under the lock until the loop's call arrives, and a walk on
  another thread can delay the pass by a frame. The bench still passed (+0.38%). An O(1) check
  (the outer return address at `[rsp+0x30]` at detour entry) was suggested and not benchmarked.
- `InfoWriter::attach` parses the schema again and allocates on every pass, on the game thread.
- The overrun count never decays, so 11 passes over 20 ms in a whole session switch a task off.
  No task was switched off as slow in any run (max pass 1.72 ms).

## Not tested and why

- Windows: not scheduled (Amendment C).
- More than one real match: owner ruling (Step 3). Abandons, reconnects, hero swaps and
  surrenders were not observed.
- Per-kill latency from a screen recording: owner ruling (Step 4, checked by eye).
- An alternated bench with the module injected in both phases: the collector can't be unloaded
  and no on/off switch was built (owner ruling, Step 2).
- The main-loop patterns across a tier0/engine2 patch: no such patch landed in the window.
- `installed_build` on 6810 with the unchanged f528948 build: not run (later builds pass; their
  changes were not patch fixes).
- The `tower_destroyed` full count: the private-lobby match's metadata never came (503 three
  times; attempts stopped), such matches may never be published, and no matchmade match was
  logged with the task.
- `match_outcome` for an Amber win, PostGame_PlayOfTheGame and Abandoned: not seen (one finished
  match with the task).
- The `game_paused` true/false event in a matchmade match: tested only in a private lobby as
  host (the older pause-only event fired twice in the real match, not compared with the display).
- Other game modes (StreetBrawl and others): `tower_destroyed` is gated to Normal, and the
  StreetBrawl scores are not read.
- Item ids: only item counts were compared.
- Proton versions other than 11.0.
- `incoming_damage` and `match_history`: unavailable (see the table).

## Decisions needed from owner

1. **Hideout/sandbox match id.** The game gives no `match_id` there, so there is no
   `match_start`/`match_end`. Overwolf's handler generates a local match id in that case; ours
   emits nothing (no invented data). Keep that, or have the provider generate a local id?
   (Pending since Step 1.)
2. **Unavailable features.** `incoming_damage`: Overwolf reads it from a HUD object at fixed
   non-schema offsets, and the game's own numbers arrive as user messages; both are outside the
   collector rules. `match_history`: not attempted. Accept both as unavailable, or choose another
   source?
3. **`tower_destroyed` accuracy.** The full count is unverified: the private-lobby match's
   metadata never came, and such matches may never be published. Accept it on the
   owner-observed subset, or verify it on a matchmade match first?
