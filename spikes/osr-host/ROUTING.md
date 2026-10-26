# Production routing fixture

This is a test receiver for input v2. The injected module chooses the widget;
the renderer applies its fixture state and completes before progress is acknowledged.
It does not inject native Chromium input. The reused Vulkan layer supplies drawing.

Build from the repository root:

```bash
cargo build --release -p uw-inject
cargo build --release --target x86_64-pc-windows-gnu -p uw-bootstrap -p uw-helper -p uw-overlay
cd spikes/osr-host
npm ci
npm run build
```

For an authorized test game session, launch the game with `UW_SPIKE_CAPTURE=win32`
and the drawing layer enabled, then run (substitute an absolute checkout path):

```bash
UW_SPIKE_CAPTURE=win32 UW_INPUT_PID=12345 npm start -- \
  --capture routing --input-command /absolute/checkout/scripts/input-3b-launch.sh
```

`UW_INPUT_PROTON` optionally selects the existing Wine distribution directory
containing `bin/wine`, for example `/path/to/Proton 11.0/files`;
`UW_INPUT_EXE` defaults to `deadlock.exe`. The PID restricts discovery to the
intended process. This mode starts the production helper/bootstrap/overlay only.

Two overlapping widgets begin at `(100,100)` and `(360,260)` in game client pixels.
Ctrl+Tab toggles exclusive mode; Ctrl+F10 cycles the second widget through the
three policies. Standard ownership leaves dimming off. Exclusive mode uses the
native cursor and the layer's dimming; no decorative cursor is drawn.

Send one JSON control per line on the Electron host's stdin:

```json
{"control":"mode","widget":1,"mode":"notify"}
{"control":"move","widget":1,"x":140,"y":120}
{"control":"rebind","vk":9,"modifiers":9}
{"control":"request","action":"takeover","widget":2}
{"control":"request","action":"release_takeover","widget":2}
{"control":"request","action":"leave_edit"}
{"control":"visible","on":false}
{"control":"reuse","widget":1}
{"control":"remove","widget":2}
```

The rebind example selects Ctrl+Shift+Tab (modifier family mask 9). Ctrl+F10
remains reserved for fixture policy cycling. `reuse` advances the widget generation;
old input is cancelled before the replacement is committed. `SIGUSR1` pauses
actual fixture consumption; `SIGUSR2` resumes it. A stalled receiver must lose
ownership through the game-local lease and must not recapture on resume.

The test bitmap is 850×560 client pixels. Move controls outside that drawing
surface are rejected before publication, preserving the previous table.

Automated checks, without starting a game:

```bash
npm run test:production-routing
npm run test:production-input
xvfb-run -a npm run test:production-routing-smoke
xvfb-run -a npm run test:production-smoke
bash -n ../../scripts/input-3b-launch.sh
```
