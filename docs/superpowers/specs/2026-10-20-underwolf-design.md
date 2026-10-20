# Underwolf — Design

**Date:** 2026-10-20
**Status:** Draft for review

## 1. What this is, and why

Underwolf is a cross-platform (Linux and Windows) desktop platform for in-game apps. It implements the same JavaScript API as Overwolf (`overwolf.*`), so a developer can publish their existing Overwolf app on Underwolf with little or no change.

**Why it exists.** Overwolf is Windows-only. The project owner plays Deadlock on Linux (through Proton) and could not use the Deadlock companion apps that show a live match view, performance stats, and player profile. Other Linux players are in the same position for other games. Underwolf fixes this, and is built as a platform for third-party developers from day one rather than as a personal tool.

**Who it serves.**
- **Players** on Linux (first priority) and Windows who want in-game companion apps.
- **App developers** who already ship on Overwolf (or want to) and want to reach Linux players without rewriting their app.

## 2. Goals for v1

v1 is "done" when all of these are true:

1. **Game integration works.** Underwolf detects Deadlock starting and stopping on Linux (Proton) and on Windows (Windows in milestone 2), and reports it through `overwolf.games` the way Overwolf does.
2. **Live game tracking works.** Apps receive live Deadlock match data through `overwolf.games.events`, aiming to match Overwolf's Deadlock feature set (see §6.4).
3. **True in-game overlay works.** App widgets render inside the game's frames (not as windows on top). No windowed fallback.
4. **Players can drag, move, resize, hide and show overlay widgets**, and the layout is remembered per game.
5. **It is smooth.** Target (owner's call to tighten): overlay costs under ~2–3% FPS; no visible stutter.
6. **Beautiful, modern, intuitive UI.** The launcher is polished and usable without a tutorial.
7. **Excellent developer documentation**, good enough that an Overwolf developer can port and publish an app using only the docs.

## 3. Non-goals for v1

- Games other than Deadlock. (The architecture must make adding games straightforward, but only Deadlock ships.)
- Video capture, replays, highlights (`overwolf.media`, `overwolf.streaming`, replay APIs).
- Underwolf accounts, ratings, reviews, comments.
- An Underwolf ad network or subscription/payment system (see §6.7).
- macOS.
- 100% coverage of every `overwolf.*` API. v1 covers what real Deadlock apps and Overwolf's sample apps actually use (see §6.2).

## 4. Anti-goals (failure even if it technically works)

- **Getting players banned.** Underwolf must never write to game memory or alter game behavior. Anything inside the game process is read/draw-only.
- **Breaking apps in subtle ways.** Apps must run in the same browser engine family (Chromium) Overwolf uses, so they behave the same.
- **Hurting game performance noticeably.**
- **An ugly or confusing UI.**
- **Inventing data.** If the Deadlock provider can't get a value, the event/field is simply not emitted. No fabricated or estimated values (Overwolf's event format has no way to label an estimate).
- **Locking developers in.** Developers keep their own ads, payments and backends.

## 5. Constraints and known facts

- **Deadlock has no native Linux build**; it runs under Proton (Windows build + Wine + DXVK/VKD3D → Vulkan).
- **Deadlock on Windows appears to default to Vulkan**; players can force DirectX 11 with `-dx11`. Both must be supported.
- **Deadlock uses VAC** (not a kernel anti-cheat). VAC targets known cheats; Overwolf, Discord, Steam overlay and OBS all inject into games. Ban risk from a read/draw-only overlay is low but not zero.
- **No official Valve live-data feed exists for Deadlock** (unlike CS2/Dota 2 Game State Integration). Overwolf ships its own Deadlock event provider; how it gets the data is undocumented.
- **The community Deadlock API** (deadlock-api.com) provides profiles, match history and a live spectator feed. The live feed runs 60–90 s behind the real game.
- **Overwolf's API is publicly documented** at dev.overwolf.com. Underwolf is a clean-room implementation against those docs. **No reverse engineering of the Overwolf client, no redistribution of Overwolf code or apps.**
- **Overwolf's sample apps** (the official basic sample app on GitHub, and the Deadlock sample linked from Overwolf's Deadlock GEP docs if available) are legitimate compatibility test targets.

## 6. The design

### 6.1 Stack (decided)

- **Shell:** Electron + TypeScript. Chosen because Overwolf apps are written and tested against Chromium; Electron ships the same Chromium on Linux and Windows. (Tauri was rejected: it uses WebKit on Linux, which would break apps.)
- **Native layer:** Rust — game detection, event providers, overlay renderer/hooks, input capture. Connected to Electron via native modules (napi-rs) and/or helper processes; the split is the builder's call.

### 6.2 App runtime (Overwolf API compatibility)

**Target platform: Overwolf's original ("native") platform** — HTML/JS apps packaged as `.opk` with `manifest.json`, using the injected `overwolf.*` global. This is what the Deadlock apps on the Overwolf Appstore are built on. Apps built for Overwolf's Electron product (ow-electron: full Electron apps that load Overwolf packages via `app.overwolf.packages`) are a different runtime model and are **out of scope for v1**; supporting them is a later milestone.


- Loads Overwolf app packages: `.opk` files and unpacked folders, reading `manifest.json` (windows, permissions, game targeting, launch events, hotkeys, etc.).
- Runs each app in its own sandboxed Chromium context and injects an `overwolf` global implementing the API, with the same names, call shapes (callback style), result objects (`{ success, error, ... }`) and event objects as Overwolf.
- **Permissions:** an app only gets APIs its manifest declares. Players see the permissions before installing.
- **v1 API scope:** decided by measuring real usage. Procedure: inventory the `overwolf.*` calls in Overwolf's Deadlock sample app and other Overwolf open-source sample apps, plus any Deadlock apps whose developers share their code, and implement that set. Expected core: `overwolf.windows`, `overwolf.games`, `overwolf.games.events`, `overwolf.settings` (incl. `hotkeys`), `overwolf.extensions` (current manifest, messaging between windows), `overwolf.utils` (e.g. open URL), `overwolf.io`/storage basics.
- **Unsupported APIs** must fail the way Overwolf reports an unsupported/failed call (a result with `success: false` and an error), never throw or hang, and must be listed in the docs.
- **Profile/login APIs** (`overwolf.profile`) return "not logged in" in v1 (no Underwolf accounts).
- **Developer mode:** load an unpacked folder or `.opk` directly, with Chromium DevTools for each app window.

### 6.3 In-game overlay (the hardest part)

**Rendering — always inside the game, no windowed fallback:**
- **Vulkan layer** (Rust). Covers Deadlock on Linux (Proton → Vulkan) and Deadlock on Windows in its default Vulkan mode. Registered through the official Vulkan layer mechanism (implicit layer manifest), the way MangoHud and the Steam overlay work. On Linux it must work inside Steam's Proton runtime container.
- **DX11 hook** (Windows only). For players who run `-dx11`. Injected into the game process the way Overwolf/Discord/OBS do it. Read/draw-only. (On Linux, DX11 is translated to Vulkan by DXVK, so the Vulkan layer covers it.)
- **Pipeline:** Electron renders each in-game app window off-screen into a GPU texture shared across processes (Electron offscreen rendering with shared textures — the same approach Overwolf's own Electron product uses). The layer/hook composites those textures over the game frame just before present, at the positions the player set. Textures must stay on the GPU (no CPU copies) for performance.

**Input — same model as Overwolf:**
- Each overlay window has a passthrough mode: `noPassThrough` (overlay takes input, game doesn't see it), `passThrough` (game gets everything), `passThroughAndNotify` (game gets it, overlay is notified). Apps can switch it at runtime.
- **Windows:** the injected module hooks the game's input path and routes each event per those modes.
- **Linux (Proton):** input reaches the game via Wine from X11/Wayland, which a Vulkan layer cannot intercept directly. The mechanism is an open technical question for Spike 1 (§8). Candidates: hooking input within the Wine/Proton process, or a short-lived input-capturing surface over the game while the overlay has focus.

**Layout editing and hotkeys (decided: match Overwolf):**
- **Ctrl+Tab** enters exclusive/edit mode, as in Overwolf: the game goes semi-transparent, a cursor appears, all input goes to the overlay. Underwolf adds layout editing in this mode: every widget gets an outline and can be dragged and resized; a widget tray toggles each widget on/off. Ctrl+Tab again (or clicking outside an app window) locks the layout and returns input to the game. Rebindable.
- Outside edit mode, widgets are display-only unless an app sets a window to `noPassThrough`.
- Layout (positions, sizes, visibility) is saved per game per app window.
- **App hotkeys** come from each app's manifest `hotkeys` section, with the same defaults, rebinding, and `overwolf.settings.hotkeys` API as Overwolf. `focus_game_takeover` / `focus_game_takeover_release_hotkey` work as on Overwolf.

### 6.4 Game events (Deadlock provider)

- Event providers are **per-game plugins** (Rust) emitting Overwolf's event format: info updates `{ feature, category, key, value }` and events `{ name, data }`, delivered via `overwolf.games.events.setRequiredFeatures`, `onInfoUpdates2`, `onNewEvents`, `getInfo`.
- **Deadlock target feature set** (from Overwolf's Deadlock GEP docs):
  - `gep_internal`: version info
  - `game_info`: `phase`, `game_mode`, `match_history`, `steam_id`
  - `match_info`: `team_score`, `match_id`, `roster`, `items`, `incoming_damage`, `match_outcome`
  - events: `match_start`, `match_end`, `kill`, `death`, `assist`, `match_clock`, `game_paused`, `tower_destroyed`
- **Data source (owner decision): read-only reading of the Deadlock process's memory is the primary source for live, real-time match data.** The 60–90 s delayed community feed does not meet the bar for live tracking. The owner accepts the VAC risk of read-only memory reading. Hard rule: **never write to game memory, never inject code for data collection, never alter game behavior.** Other sources supplement it where they are cleaner: local game outputs (console log, state files) and the community Deadlock API for profile, match history and post-match data.
- On Linux the reader runs natively and reads the Proton/Wine game process (e.g. `process_vm_readv` / `/proc/<pid>/mem`); on Windows it uses read-only process access. Game updates will move memory layouts, so the provider must be built to be updated quickly (signature/pattern-based offsets, versioned per game build) and must fail safe: if layouts don't match, stop emitting rather than emit wrong data.
- Every feature the provider supports is reported via `getInfo`/feature status so apps can tell what's available; unsupported features are reported as such, not faked.
- Adding a new game = adding a provider plugin plus a game entry (ID, process detection, renderer). Provider-authoring is documented for the community.

### 6.5 Catalog and distribution of apps (decided)

- **Open catalog in a public Git repository** (Obsidian community plugins / Flathub model).
- **Developer flow:** add an entry (app ID, name, supported games, link to a signed release, e.g. GitHub Releases) and open a pull request. Automated checks validate the manifest, declared permissions, package signature and download. A human reviews the first submission only; later updates flow automatically when the developer publishes a new release.
- **Packages are signed** by the developer; Underwolf verifies the signature on install and update.
- **Removal:** deleting a catalog entry delists the app; the launcher stops offering it (whether already-installed copies are disabled is the builder's call, default: warn the player).
- No store server or accounts in v1. A hosted store can be layered on the same catalog later.

### 6.6 Launcher UI (decided: store-first, "A")

- Icon rail + sidebar layout. Opens on **Discover** (featured app hero + grid), with sections by game (Deadlock, All games) and **Library** (My apps, Updates). Settings one click away.
- App detail page shows description, screenshots, supported games, and permissions before install.
- Status of the running game and overlay is visible (e.g. "Deadlock running · overlay active").
- Each app keeps its own desktop windows, as on Overwolf; the launcher is the store and manager, not the apps' UI.
- Visual quality bar: modern, polished, dark theme first, smooth. The specific visual design is the builder's call within this layout.

### 6.7 Monetization (decided)

- Developers bring their own ads, payments and backends; Underwolf places no restrictions on them beyond the permission model.
- Overwolf's ad and subscription APIs (`overwolf.ads`-style APIs, Overwolf Subscriptions/Tebex APIs) are **stubbed**: they load and respond without errors, show no ads, and report "no active subscription".
- The stubs sit behind a **pluggable provider interface**, so an Underwolf ad network or payment integration can be connected later without app code changes.

### 6.8 Developer documentation

Required for v1:
- **Porting guide** from Overwolf: what works unchanged, what's stubbed, what's unsupported, how to test in developer mode.
- **API reference**: every supported `overwolf.*` call, with any behavioral differences from Overwolf noted.
- Manifest reference (the subset Underwolf reads).
- Publishing guide for the catalog (signing, PR, review, updates).
- Monetization guide (bring your own; what the stubs do).
- **Game provider authoring guide** for adding new games.
- A docs website (static site, hosted for free, e.g. GitHub Pages); the tool is the builder's call.

## 7. Distribution of Underwolf itself

- **Windows:** signed installer (code-signing is required so Windows Defender doesn't flag the DX11 injector), with auto-update. **No certificate exists yet.** Start acquiring one (OV/EV, needs a legal person or company, can take weeks) early in milestone 1 so it doesn't block milestone 2. Unsigned builds are fine for development only.
- **Linux:** a format that can install the Vulkan implicit layer where Steam/Proton picks it up. The specific format is the builder's call after Spike 1/2 (note: a sandboxed Flatpak may not be able to install a host Vulkan layer or see the game process; AppImage + a user-level layer install, or distro packages, may be needed).
- Auto-update on both platforms.

## 7a. Supported environments for v1

- GPUs: NVIDIA (proprietary driver) and AMD (Mesa). Intel best-effort.
- Linux: KDE Plasma and GNOME, Wayland sessions (games run through XWayland or native Wayland). *Updated 2026-10-20 (owner): current GNOME (50+) and KDE Plasma (6.8+) no longer offer X11 sessions.*
- Windows: 10 and 11.
- Not in v1: Steam Deck / Gamescope session, wlroots-based compositors (best-effort), dual-GPU laptops (best-effort).

## 8. Technical spikes (do these first; the plan depends on them)

1. **Linux input under Proton.** Prove the overlay can capture mouse/keyboard in edit mode and pass it back to Deadlock afterwards, on KDE and GNOME Wayland sessions (the game itself runs through XWayland).
2. **Shared-texture compositing.** Prove an Electron offscreen-rendered texture can be composited by (a) the Vulkan layer under Proton and on Windows, and (b) the DX11 hook, within the performance target.
3. **Deadlock live data via read-only memory reading.** Prove the §6.4 features can be read from the game process on Linux (Proton) and Windows, and how robust the approach is across game updates. Output: a feature-by-source table, noting what comes from memory vs. other sources.

If a spike fails, stop and bring the finding back to the owner before re-planning. Do not quietly add a windowed fallback or a memory-writing workaround.

## 9. Decisions log

**Made by the owner:**
- Build a new platform (not reverse-engineering Overwolf), API-compatible with Overwolf, for third-party developers from day one.
- Linux and Windows. **Linux ships first** (milestone 1: Linux, Vulkan layer); **Windows follows** (milestone 2: Windows Vulkan + DX11 hook, Windows input hooking, signed installer).
- Deadlock live data comes from read-only game memory reading; owner accepts the VAC risk.
- No Windows code-signing certificate yet; one must be obtained before the Windows release (see §7).
- Stack: Electron + TypeScript + Rust.
- Deadlock is the launch game.
- True in-game rendering in v1, including DX11; no windowed fallback.
- Edit mode via hotkey, using Overwolf's Ctrl+Tab; app hotkeys behave exactly as on Overwolf.
- Developers bring their own ads/payments; Overwolf ad/subscription APIs stubbed but pluggable.
- Open Git-based catalog + developer mode.
- Store-first launcher layout (A).
- Project name: Underwolf. Location: ~/Dev/Underwolf.

**Made by Claude (owner to check):**
- Underwolf is open source. License: MIT (to confirm).
- v1 targets Overwolf's native platform only; ow-electron apps are a later milestone.
- v1 API scope = what Overwolf sample apps and real Deadlock apps use, not full coverage.
- No estimated values in events (not emitted instead).
- Overlay performance target: under ~2–3% FPS.
- Unsupported APIs return Overwolf-style failure results; `overwolf.profile` reports "not logged in".
- Delisted apps: warn the player, don't force-remove.
- Docs hosted as a static site.

## 10. Left to the builder

- Process architecture (which parts are native modules vs. helper processes; IPC design).
- Internal data formats for layout storage and settings.
- Catalog repository structure and CI checks implementation.
- Signing scheme details (key format, tooling).
- Exact visual design within the store-first layout.
- Linux packaging format (after spikes).
- Test strategy, with the constraint that API compatibility is tested against Overwolf's sample apps.
