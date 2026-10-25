# Input 3b — configurable hotkeys and widget input routing

**Date:** 2026-10-25
**Status:** Approved by the owner on 2026-10-25 after the builder review. The
behavior and limits below originated as the author's proposals.
**Starting point:** `main` at `28996ad`, following the integration of input 3a.

## Intent

Complete roadmap item 3's remaining input work. A player can rebind the overlay
toggle, interact with the intended widget, and return to the game with the same
release, cursor restoration and restart guarantees established in 3a. An app's
window can block input, pass it to the game, or receive notifications while the
game continues to receive it.

Keep Linux/Proton as the delivery target. Windows ABI code is needed inside
Proton; a Windows product release remains unscheduled. The named-pipe/helper
architecture, MinHook and the read/draw-only boundary remain in force.

## Decisions inherited from existing designs

- The injected module owns hit-testing and input decisions. The host sends the
  hotkey table and widget layout; it does not decide whether to block an event
  after receiving it.
- 3b owns configurable hotkeys, widget targeting, the three passthrough modes,
  DirectInput, GameInput, and a low-level fallback if thread hooks need it.
- No automatic recapture after focus loss, hiding, delivery loss or reconnect.
  Fresh input or a fresh accepted host request is required.
- No game data/code changes, calls to game functions, remote-thread injection,
  or production dependencies on spike crates.
- Dirty-rectangle frame transport and the full overlay performance gate remain
  roadmap item 4. The provider port and the app runtime come later.

## Approaches

1. **Host tables with in-module routing — recommended, following the existing
   architecture decision.** Extend the portable input policy and current relay.
   Every game-facing input API consults the same ownership decision. The cost is
   a new input protocol version and more policy state.
2. **Host-side hit-testing after forwarding.** It could reuse more spike code,
   but cannot synchronously prevent input reaching the game and makes a stalled
   receiver responsible for suppression. This conflicts with the inherited
   in-module hit-testing decision and 3a's local release guarantee.

Implement 3b in ordered steps within one design: tables/protocol, portable
routing, native adapters, then integrated acceptance. Do not postpone the
DirectInput/GameInput evidence and label the routing subset a completed 3b.

## Scope

Included:

- A host-supplied named hotkey table, with Ctrl+Tab as the initial binding for
  `toggle_edit_mode`; live replacement and named activation notifications.
- Versioned widget tables containing identity, bounds, stacking, visibility,
  keyboard eligibility and input mode.
- In-module pointer targeting, keyboard focus, paired button/key delivery,
  pointer capture during an interaction, and explicit cancellation.
- Standard interaction and exclusive edit mode with `noPassThrough`,
  `passThrough` and `passThroughAndNotify` policy.
- DirectInput and GameInput isolation adapters with explicit support reports
  and tests of their actual interfaces.
- A multi-widget receiver using the existing drawing-only test adapter, plus
  isolated native probes and an owner-observed Deadlock check.

Excluded:

- The `overwolf.*` JavaScript API, manifest/package loader, launcher/settings UI,
  persistent layouts, widget tray and production drag/resize controls.
- Full IME composition, touch/pen input, controller navigation in widgets,
  XInput isolation, 32-bit support and additional games.
- Changes to frame transport, collector behavior or game settings.

The test receiver may provide controls/fixtures to move, hide and change modes
of widgets. These exercise routing; they do not implement the future layout UI.
Existing Electron, Rust and MinHook dependencies remain the starting stack.

## Configuration and protocol

**Compatibility policy:** matching 3b input-capable host/helper/DLL builds are
required; reject input v1/v2 mixing clearly. This was the author's recommended
default, accepted with the design approval. The bootstrap, legacy state-only
path and game-data layouts retain their existing contracts.

Use input protocol v2, with explicit negotiation and reports for routing,
hotkeys and native API coverage. Do not reinterpret v1 records or the existing
64-byte state block. A failed negotiation leaves capture unavailable, with
normal game input and a diagnostic explaining the mismatch.

Publish a complete configuration snapshot, scoped to the current input epoch
and a monotonically increasing configuration revision. Validate it outside the
hook path, queue it to the UI owner, then apply it atomically. The application
acknowledgement means the owner has committed it, rather than the helper having
written it. Reject stale epochs/revisions and malformed snapshots without
partially changing the live table.

Proposed initial bounds: 32 widget instances, 64 hotkey bindings, and one
configuration message within the existing 4096-byte pipe payload limit. Keep
names on the host and use numeric identifiers in the binary tables. The plan
must show the exact encoded maximum before implementation; reduce the limits
or explicitly redesign transport if it cannot fit. Never silently truncate.

Each routed event identifies its widget instance and configuration revision;
an instance includes a generation so reusing a numeric slot cannot receive old
input. Local coordinates are calculated against the snapshot used at ingress.
An event routed before a geometry update may still reach that same live
instance; the receiver must not drop its key/button release merely because a
newer geometry revision exists. Removal/replacement is an ordered cancellation
barrier. Hotkey events identify the binding and triggering edge.

Retain bounded nonblocking output, per-epoch sequence numbers, the existing
100 ms heartbeat and 500 ms delivery lease. Consume routed input in the actual
receiver before acknowledging progress. Status, cancellation and restoration
must not depend on space in a mouse-motion queue. Motion coalescing is allowed
only for adjacent moves to the same instance under the same routing snapshot.
Discrete-event overflow revokes ownership as in 3a.

Tables can be republished after reconnect; they restore configuration, not
ownership or held-state history. No queued command or table can restore a
previous capture session.

## Hotkeys

Match the table against real physical state before game-facing filtering, with
one canonical source so raw and legacy delivery cannot fire a binding twice.
Use virtual keys plus explicit modifier requirements. An unsided modifier
accepts either side. Additional modifiers do not match unless the binding
explicitly permits them. Key repeat does not retrigger a toggle; replacing the
table while its chord is held does not create a new activation.

Each binding supplies a stable host identity, chord, action and passthrough
flag. Initial actions are toggle edit mode, activate a named host action, and
request/release takeover for a widget. The host owns names and conflict
reporting. Reject duplicate identities, conflicting chords and invalid target
references before applying a snapshot. Exactly one edit-mode binding must
remain configured so a successful rebind also leaves an exit route.

For a consuming hotkey, consume the triggering key and its matching release;
retain 3a's held-key accounting for modifiers the game had already observed.
For a passthrough hotkey, notification adds no suppression outside an existing
takeover. Losing focus clears chord state; focus return while a chord is held
cannot fire it until release and a fresh press.

Widget takeover enters the same exclusive isolation mode as the edit toggle,
with the named widget as its keyboard target and takeover owner. It does not
start layout manipulation. The target must already be visible and eligible;
an unavailable request fails immediately rather than waiting for a later show.
A release for the current takeover owner exits exclusive mode. A stale release
or a release naming another owner is rejected. A fresh takeover for a different
widget transfers focus with cancellation of the old target's held state.
Hiding/removing the takeover owner exits exclusive mode; hiding an ordinary
focused widget in globally toggled edit mode clears focus and leaves that mode
available for interaction with the other widgets.

The future app runtime can translate manifest hotkeys and focus-takeover flags
to these internal actions. This phase does not claim that API is implemented.

## Routing and ownership

Keep connection/capture lifecycle separate from the routing decision. The
policy records exclusive mode, pointer target, keyboard focus, button/key
ownership, and which input domains the overlay currently owns. A single global
`active` boolean is insufficient for standard interaction and notification
windows. Extend actual-state reporting without changing the meaning of old
messages in place.

Bounds use physical game-client pixels and half-open rectangles. Highest
stacking order wins; equal stacking order is rejected as ambiguous. Hidden,
zero-size or wholly out-of-client widgets are ineligible. Clip partially visible
widgets to the client. A `passThrough` widget does not obstruct an interactive
widget below it. Alpha/pixel hit masks are deferred; rectangles are explicit.

| Mode outside exclusive editing | Pointer input in the eligible widget | Keyboard/text input |
|---|---|---|
| `noPassThrough` | Deliver to the widget and block the corresponding game input | Deliver and block only while this widget owns keyboard focus |
| `passThrough` | Deliver to the game; no widget notification | Deliver to the game; no widget notification |
| `passThroughAndNotify` | Deliver to the game and the targeted widget | Notify only when explicitly keyboard-focused; game input continues |

A pointer down can grant keyboard focus to an eligible widget. Pointer capture
pins an interaction to its original instance until its final button release,
even when the pointer leaves its bounds. Key releases follow the instance that
received their down, rather than whichever widget is focused later. Focus
transfer first releases/cancels held state in the old target. Text follows the
accepted keyboard target and the existing UTF-16 decoder; no second text
synthesis path is introduced.

Outside exclusive mode, a fresh pointer down whose effective target is the
game clears widget keyboard focus and cancels its held state before forwarding
the click to the game. This includes a `passThrough` area with no eligible
widget underneath it. An existing pointer capture takes precedence until its
buttons are released. Continue suppressing already-owned physical keys until
their release, so focus transfer cannot turn a held key into a new game press.

Notification mode does not unclip/show/warp the cursor, release game-held keys
or alter polling APIs. Standard blocking interaction suppresses only its owned
input domains; the same decision is used by messages, raw input, key polling,
cursor APIs and the new adapters. In an FPS state with a confined/hidden
pointer, interaction requires exclusive mode; hover does not silently create
exclusive capture or move the pointer out of the game.

Exclusive mode retains 3a's all-input isolation and native pointer feedback.
It makes visible widget bodies eligible for interaction regardless of their
standard-mode passthrough setting. A background click exits exclusive mode and
is consumed, including its paired release, so it cannot fire into the game.
The rebound toggle also exits. Initial keyboard focus goes to the explicitly
requested eligible widget; otherwise keyboard input waits for a widget click.

Hiding/removing/replacing an owned widget, changing its blocking mode, or
invalidating the client geometry cancels affected interactions and restores
the game-facing state. Movement alone preserves the target instance. Foreground
loss, global hide, lease failure or disconnect revokes all blocking ownership
and clears focus/capture. Retain 3a's held-until-release suppression and
owner-thread restoration, including its stalled-UI boundary.

After such a loss, a stationary pointer over a blocking widget cannot recapture
by itself. Require a fresh click, hotkey or accepted host request after the
receiver and table are ready. Applying configuration or showing widgets is not
that request. Passive notification can resume without taking game input.

## DirectInput and GameInput

The native adapter work begins by proving ABI discovery and callable original
methods in an isolated process. Resolve real system exports and methods, as
3a's import-stub correction requires. MinHook remains the hook engine; keep
trampolines and callback state alive for the game session. Do not hook under
`DllMain`, patch game code, or run API calls while holding the policy lock.

**DirectInput:** target DirectInput 8 keyboard/mouse devices, both A/W interfaces.
Cover device creation, acquisition intent, immediate reads and buffered reads,
including aliases and devices created before overlay injection. The game sees
the relevant device as unacquired during blocking ownership. Its latest
successful acquire/unacquire intent is retained; release must not reacquire a
device the game requested unacquired. Preserve original failure behavior,
custom data-format bounds, buffer counts, `DIGDD_PEEK`, flush semantics and
unrelated devices. Captured buffered transitions must not replay on release.

**GameInput:** explicitly identify the supported DLL/interface ABI instead of
assuming every version shares a vtable. Cover synchronous current/history
readings, reading objects retained by the game, and asynchronous reading
callbacks. During exclusive capture, freeze the game-facing input stream at a
neutral boundary so a pre-existing held action cannot persist; standard mode
filters only owned keyboard/mouse domains. Do not replay captured history or
leak captured values through an alternate reading accessor after release.
Preserve object identity/refcounts and callback ordering, cancellation and
lifetime rules. Never unregister a callback synchronously from inside itself.

This does not add controller delivery to widgets or XInput coverage. The plan
must specify which GameInput domains freeze in exclusive mode and test them;
it must not advertise whole-controller isolation if another API is untouched.

Optional DLLs may be absent in Proton. Their absence is a reported coverage
state, not a passing runtime test and not a reason to install a replacement
runtime into the player's game. Unknown present ABIs or an uncovered active
input path make the corresponding isolation capability unavailable. Handle
late loading, already-created objects and teardown explicitly. A native probe
must demonstrate the claimed path before the phase is called complete; an
unavailable runtime leaves that acceptance item open for an owner decision.

Keep thread hooks as the initial source. Add a low-level fallback only if a
probe demonstrates a missing required path. It must be scoped to the verified
foreground game and preserve single-source delivery; it must never swallow
desktop input after the game loses focus.

## Code boundaries

| Area | Change |
|---|---|
| `uw-pipe-proto` | Input v2 configuration, targeted records, hotkey/state/capability messages and strict limits |
| `uw-input` portable modules | Hotkey matcher, immutable routing tables, focus/pairing/ownership policy and cancellation |
| `uw-input::win` | Shared domain policy in existing adapters; DirectInput/GameInput discovery and detours |
| `uw-game-channel`, `uw-overlay` | Negotiation, ordered configuration application and application acknowledgements |
| `uw-helper` | Bounded configuration and routed-event relay; coverage/version diagnostics |
| `uw-host`, `uw-inject` | Table publication and actual routing state; no stale ownership replay |
| Test game and `spikes/osr-host` adapter | Multiple input targets, real delivery fixtures and drawing-only feedback |

The implementation plan owns exact structs, message tags, method discovery,
memory ownership, dependency/binding additions and sequencing. It must resolve
native ABI versions, pre-existing objects and hook coverage before product
implementation, and retain separate native and simulator evidence.

## Verification and completion

1. Portable tests cover rebind while held, both modifier sides, exact modifiers,
   repeated input, conflicts, passthrough hotkeys and one activation per edge.
2. Routing tests cover overlap, stacking, bounds, visibility, each mode, local
   coordinates, keyboard focus, pointer capture, paired releases, removal,
   reused identifiers, geometry changes and consumed background-click exit.
3. Protocol/relay tests cover all bounded limits, atomic replacement, stale
   revisions/epochs, version rejection, actual application acknowledgements,
   target-aware coalescing, overflow and receiver-consumption challenges.
4. Real isolated API probes exercise DirectInput immediate/buffered reads and
   GameInput polling/history/retained readings/callbacks for each claimed ABI,
   including pre-existing objects, late loading and capture/release cycles.
   Mocks alone do not establish native adapter coverage.
5. Extend 3a's helper-loss, receiver-stall, stdout-backpressure, hidden-window,
   foreground-loss and destruction checks to active widget interactions. No
   stale focus, drag, key or takeover may survive into a new session.
6. Fresh Deadlock Hideout/bot checks demonstrate a rebound toggle, independent
   text/click/wheel delivery to multiple widgets, all passthrough modes, held
   key recovery, consumed background exit and loss/restart recovery. Record
   whether standard-mode interaction is in a game state with a visible cursor.
   Do not run overlay acceptance in a real match.
7. Native and Windows-target build/test/clippy gates and the existing injection,
   communication and data-channel regressions pass. Preserve the separation
   between automated evidence and owner observations.
8. Compare 3b enabled-but-idle input overhead against 3a under the same settled
   scene/procedure, record active-interaction hitches and release latency, and
   retain raw evidence and rejected/incomplete attempts. The complete overlay
   performance budget is still item 4's gate.

Record the tested Proton build, desktop, game build, artifact revisions,
resolution/DPI and native API availability. Untested desktop/runtime variants
remain explicitly untested. 3b completion requires both integrated acceptance
and evidence for each advertised native API capability.

## Sources and limits

- Existing decisions: [parent design](2026-10-20-underwolf-design.md),
  [input 3a design](2026-10-24-input-3a-design.md),
  [3a acceptance](../checks/2026-10-24-input-3a.md) and
  [milestone order](../../spikes/SUMMARY.md).
- Current code: the portable policy and Windows adapters in `crates/uw-input`,
  input v1 protocol and session, host receiver, and test-only production surface.
- Context7 was queried on 2026-10-25 for Overwolf and Microsoft Win32. It returned
  relevant exclusive-mode guidance but Electron-platform passthrough material
  and unrelated DirectInput/GameInput results. Those do not establish native
  platform compatibility or a usable ABI.
- [Native exclusive-mode guide](https://dev.overwolf.com/ow-native/guides/general-tech/exclusive-mode-overlay/)
  informs the toggle and background-click behavior. [Native manifest reference](https://dev.overwolf.com/ow-native/reference/manifest/manifest-json/)
  distinguishes clickthrough, simultaneous input notification, keyboard focus
  and hotkey passthrough. The three mode names here are Underwolf's inherited
  internal policy; their future native API mapping is separate runtime work.
- [DirectInput acquisition](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ee417818(v=vs.85))
  and [buffered reads](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ee417894(v=vs.85))
  constrain acquisition and buffer handling.
- [GameInput readings](https://learn.microsoft.com/en-us/gaming/gdk/_content/gc/input/overviews/input-readings),
  [callback lifecycle](https://learn.microsoft.com/en-us/gaming/gdk/docs/features/common/input/advanced/input-callbacks),
  [API version history](https://learn.microsoft.com/en-us/xbox/gdk/docs/reference/input/gameinput/gameinput_members)
  and [focus policy](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/input/gameinput/interfaces/igameinput/methods/igameinput_setfocuspolicy)
  motivate explicit ABI coverage and testing beyond only current-reading polls.
  These references do not establish Proton behavior.

## Builder check and approval

2026-10-25: a separate read-only reviewer requested definitions for widget
takeover and background-click focus clearing. Both are now explicit above;
these resolutions are the author's proposals. The same reviewer reread the
changes and reported "nothing important left" for making an implementation plan.

The owner approved the reviewed design on 2026-10-25 and authorized the
implementation plan. No product code, dependencies, game processes or game
settings were changed to prepare the design.
