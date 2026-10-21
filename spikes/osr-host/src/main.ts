// Electron offscreen-rendering host for the overlay spikes.
//
// Renders each widget in a hidden offscreen BrowserWindow (CPU bitmap mode) and hands every frame
// to the Vulkan layer through the native bridge, via shared-memory slots. Electron's GPU
// shared-texture mode delivers empty frames on the NVIDIA driver; the owner chose this CPU copy
// instead (plan Amendment A, docs/spikes/02-shared-texture.md).
//
// Edit mode (Spike 1): input comes from one of three capture candidates:
//  - C2 (--capture wine, default): the layer filters input out of the game in Wine's X11 driver,
//    keeps the game focused, and draws its own cursor; it reports Ctrl+Tab (H1) and the input.
//  - C1 (--capture surface): a transparent always-on-top window over the game (fails P6: the game
//    loses focus).
//  - C3 (--capture win32): an in-game Win32 hook (injected DLL) owns Ctrl+Tab and input and
//    forwards them over localhost TCP; edit mode is DLL-authoritative, and the host accumulates an
//    absolute cursor from the hook's relative mouse motion and relays it to the layer to draw.
//    The host injects the DLL itself, Overwolf-style, each time a game's layer connects
//    (uw-spike-inject, installed by spikes/inject/install.sh).
// Drag a widget by the bar above it; clicks and typing inside a widget go to that widget. For
// wine/surface, Ctrl+Tab, Escape or a click outside every widget leaves edit mode; for win32 only
// the hook leaves edit mode. Runs on X11 (XWayland) so the surface can be positioned.
//
// Spike 3 (--gep <args>, with --capture win32): each module Hello is also answered with LoadPlugin
// for the collector (channel 1, start args <args>, may be ""); run `uw-gep watch --no-inject`.
//
//   npm start -- [--consumer] [--capture wine|surface|win32] [--gep <args>] [--widget solid|animated|static|input]...

import { app, BrowserWindow, ipcMain, screen } from 'electron';
import { spawn } from 'node:child_process';
import * as os from 'node:os';
import * as path from 'node:path';

type SendResult = 'acked' | 'dropped' | 'timeout';

interface Bridge {
  startServer(socketPath: string): void;
  /// Each in-game module `Hello` is answered with `ModuleConfig { input_hooks }`, then
  /// `LoadPlugin` when `plugin` is given (`path` is a Windows path).
  startInputServer(port: number, inputHooks: boolean, plugin?: { path: string; channel: number; args: string }): void;
  setCursor(x: number, y: number): boolean;
  peerGeneration(): number;
  takeEditRequests(): boolean[];
  takeInputs(): {
    kind: 'move' | 'button' | 'wheel' | 'key';
    x: number;
    y: number;
    button: number;
    down: boolean;
    dx: number;
    dy: number;
    code: number;
    text?: string | null;
  }[];
  swapchainSize(): { width: number; height: number } | null;
  setEditMode(on: boolean): boolean;
  setLayout(widget: number, x: number, y: number, visible: boolean): boolean;
  sendBitmap(b: {
    widget: number;
    x: number;
    y: number;
    width: number;
    height: number;
    stride: number;
    pixelFormat: string;
    data: Buffer;
  }): Promise<SendResult>;
}

const bridge: Bridge = require(path.join(__dirname, '..', 'native', 'osr_bridge.node'));

const SOCKET = path.join(os.homedir(), '.cache', 'underwolf-spike', 'overlay.sock');
/// Must match TITLE_BAR in spikes/vk-layer/src/overlay.rs.
const TITLE_BAR = 24;

const SIZES: Record<string, { width: number; height: number }> = {
  solid: { width: 400, height: 300 },
  animated: { width: 400, height: 300 },
  static: { width: 300, height: 200 },
  input: { width: 400, height: 140 },
};

interface Widget {
  id: number;
  name: string;
  /// Swapchain pixels.
  x: number;
  y: number;
  width: number;
  height: number;
  win?: BrowserWindow;
}

type Capture = 'wine' | 'surface' | 'win32';

function parseArgs(argv: string[]): { consumer: boolean; capture: Capture; widgets: string[]; gep?: string } {
  const widgets: string[] = [];
  let consumer = false;
  let capture: Capture = 'wine';
  let gep: string | undefined;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--consumer') consumer = true;
    // The value may be empty (`--gep ""`), so only its presence is checked.
    else if (argv[i] === '--gep' && i + 1 < argv.length) gep = argv[++i];
    else if (argv[i] === '--widget' && argv[i + 1]) widgets.push(argv[++i]);
    else if (argv[i] === '--capture' && argv[i + 1]) {
      const c = argv[++i];
      capture = c === 'surface' ? 'surface' : c === 'win32' ? 'win32' : 'wine';
    }
  }
  return { consumer, capture, widgets: widgets.length ? widgets : ['animated'], gep };
}

/// The C3 hook connects here (must match UW_SPIKE_HOST_PORT / the hook's default).
const HOST_PORT = Number(process.env.UW_SPIKE_HOST_PORT ?? 48610);

/// C3: inject the hook DLL into the running game. Called when a game's layer connects, which means
/// the game is up. Injecting a DLL that is already loaded is a no-op in the game.
const INSTALL_DIR = path.join(os.homedir(), '.local/share/underwolf-spike');
const INJECT = path.join(INSTALL_DIR, 'uw-spike-inject');
/// Spike 3: the collector plugin on GEP data channel 1 (uw-gep's default channel).
const GEP_CHANNEL = 1;
/// A Unix path as Wine's Z: drive sees it (uw_spike_inject::to_wine_path).
function toWinePath(p: string): string {
  return 'Z:' + p.replace(/\//g, '\\');
}
let injecting = false;
function injectHook() {
  if (injecting) return;
  injecting = true;
  console.log(`[osr-host] C3: injecting via ${INJECT}`);
  const child = spawn(INJECT, [], { stdio: 'inherit' });
  child.on('error', (e) => {
    injecting = false;
    console.error(`[osr-host] C3: could not run ${INJECT}: ${e.message} (run spikes/inject/install.sh)`);
  });
  child.on('exit', (code) => {
    injecting = false;
    console.log(`[osr-host] C3: injector exited with ${code}${code === 0 ? ' (hook loaded)' : ''}`);
  });
}

/// Hard-coded starting layout: widgets stacked down the left side, starting at (100, 100).
function layout(names: string[]): Widget[] {
  let y = 100;
  return names.map((name, id) => {
    const size = SIZES[name];
    if (!size) throw new Error(`unknown widget '${name}' (known: ${Object.keys(SIZES).join(', ')})`);
    const w: Widget = { id, name, x: 100, y, ...size };
    y += size.height + 50;
    return w;
  });
}

const counts: Record<SendResult, number> = { acked: 0, dropped: 0, timeout: 0 };
let bitmapMs = 0;
let bitmapFrames = 0;
const widgets: Widget[] = [];

function startWidget(w: Widget, consumer: boolean) {
  const win = new BrowserWindow({
    show: false,
    width: w.width,
    height: w.height,
    transparent: true,
    backgroundColor: '#00000000',
    webPreferences: { offscreen: true },
  });
  w.win = win;
  win.webContents.setFrameRate(60);
  win.webContents.on('paint', async (_e, _dirty, image) => {
    const t0 = performance.now();
    const { width, height } = image.getSize();
    // Linux NativeImage bitmaps are BGRA, premultiplied alpha.
    const data = image.toBitmap();
    bitmapMs += performance.now() - t0;
    bitmapFrames++;
    if (width === 0 || height === 0) return;
    const result = await bridge.sendBitmap({
      widget: w.id,
      x: w.x,
      y: w.y,
      width,
      height,
      stride: width * 4,
      pixelFormat: 'bgra',
      data,
    });
    counts[result]++;
    if (consumer && result === 'acked') app.quit();
  });
  win.loadFile(path.join(__dirname, '..', 'widgets', `${w.name}.html`));
}

// ---- Edit mode -------------------------------------------------------------------------------

let editOn = false;
let captureMode: Capture = 'wine';
let surface: BrowserWindow | undefined;
/// C3: absolute edit-mode cursor (swapchain px), accumulated from the hook's relative motion.
let winCursor = { x: 0, y: 0 };
let winCtrl = false;
let winShift = false;
let dragging: { w: Widget; dx: number; dy: number } | undefined;
let focused: Widget | undefined;
let pointerOver: Widget | undefined;

function createSurface() {
  surface = new BrowserWindow({
    show: false,
    frame: false,
    transparent: true,
    backgroundColor: '#00000000',
    hasShadow: false,
    skipTaskbar: true,
    resizable: false,
    webPreferences: { nodeIntegration: true, contextIsolation: false },
  });
  surface.setAlwaysOnTop(true, 'screen-saver');
  surface.loadFile(path.join(__dirname, '..', 'widgets', 'surface.html'));
}

function setEdit(on: boolean, why: string) {
  if (on === editOn) return;
  editOn = on;
  bridge.setEditMode(on);
  console.log(`[osr-host] edit mode ${on ? 'ON' : 'OFF'} (${why}) at ${new Date().toISOString()}`);
  if (captureMode !== 'surface' || !surface) {
    if (on && captureMode === 'win32') {
      // The hook sends relative motion; start the cursor centered and tell the layer to draw it.
      const sw = bridge.swapchainSize() ?? { width: 2560, height: 1440 };
      winCursor = { x: sw.width / 2, y: sw.height / 2 };
      bridge.setCursor(winCursor.x, winCursor.y);
    }
    if (!on) {
      dragging = undefined;
      if (pointerOver) leaveWidget(pointerOver);
    }
    return;
  }
  if (on) {
    // The game has the pointer, so the cursor is on the game's monitor.
    const display = screen.getDisplayNearestPoint(screen.getCursorScreenPoint());
    surface.setBounds(display.bounds);
    surface.show();
    surface.moveTop();
    surface.focus();
  } else {
    dragging = undefined;
    if (pointerOver) leaveWidget(pointerOver);
    surface.hide();
  }
}

/// Surface (window) coordinates → swapchain pixels; same as uw_spike_ipc::window_to_swapchain.
function toSwapchain(x: number, y: number, vw: number, vh: number): [number, number] {
  const sw = bridge.swapchainSize() ?? { width: vw, height: vh };
  if (vw === 0 || vh === 0) return [0, 0];
  return [(x * sw.width) / vw, (y * sw.height) / vh];
}

function hit(x: number, y: number): { w: Widget; part: 'bar' | 'body' } | undefined {
  for (const w of [...widgets].reverse()) {
    if (x >= w.x && x < w.x + w.width) {
      if (y >= w.y && y < w.y + w.height) return { w, part: 'body' };
      if (y >= w.y - TITLE_BAR && y < w.y) return { w, part: 'bar' };
    }
  }
  return undefined;
}

function mouse(w: Widget, type: 'mouseDown' | 'mouseUp' | 'mouseMove', x: number, y: number, button = 0) {
  const buttons = ['left', 'middle', 'right'] as const;
  w.win?.webContents.sendInputEvent({
    type,
    x: Math.round(x - w.x),
    y: Math.round(y - w.y),
    button: buttons[button] ?? 'left',
    clickCount: type === 'mouseMove' ? 0 : 1,
  });
}

function leaveWidget(w: Widget) {
  w.win?.webContents.sendInputEvent({ type: 'mouseLeave', x: 0, y: 0 });
  pointerOver = undefined;
}

interface SurfaceEvent {
  type: 'down' | 'up' | 'move' | 'wheel' | 'keydown' | 'keyup';
  vw: number;
  vh: number;
  x?: number;
  y?: number;
  button?: number;
  dx?: number;
  dy?: number;
  key?: string;
  ctrl?: boolean;
}

function onKey(down: boolean, key: string, ctrl: boolean, text?: string) {
  // For C3 the in-game hook owns entering and leaving edit mode, so the host must not self-exit
  // (that would desync the dim/cursor from the hook, which is still swallowing input).
  if (captureMode !== 'win32' && down && (key === 'Escape' || (key === 'Tab' && ctrl))) {
    setEdit(false, key === 'Escape' ? 'Escape' : 'Ctrl+Tab');
    return;
  }
  if (!focused?.win) return;
  const wc = focused.win.webContents;
  if (down) {
    wc.sendInputEvent({ type: 'keyDown', keyCode: key });
    const ch = text ?? (key.length === 1 ? key : undefined);
    if (ch) wc.sendInputEvent({ type: 'char', keyCode: ch });
  } else {
    wc.sendInputEvent({ type: 'keyUp', keyCode: key });
  }
}

/// x, y in swapchain pixels.
function onPointer(type: 'down' | 'up' | 'move', x: number, y: number, button = 0) {
  const target = hit(x, y);
  switch (type) {
    case 'down':
      if (!target) {
        if (captureMode !== 'win32') setEdit(false, 'click outside widgets');
        return;
      }
      if (target.part === 'bar') {
        dragging = { w: target.w, dx: x - target.w.x, dy: y - target.w.y };
      } else {
        focused = target.w;
        target.w.win?.webContents.focus();
        mouse(target.w, 'mouseDown', x, y, button);
      }
      return;
    case 'up':
      if (dragging) dragging = undefined;
      else if (target?.part === 'body') mouse(target.w, 'mouseUp', x, y, button);
      return;
    case 'move':
      if (dragging) {
        const w = dragging.w;
        w.x = Math.round(x - dragging.dx);
        w.y = Math.round(y - dragging.dy);
        bridge.setLayout(w.id, w.x, w.y, true);
        return;
      }
      if (pointerOver && pointerOver !== target?.w) leaveWidget(pointerOver);
      if (target?.part === 'body') {
        pointerOver = target.w;
        mouse(target.w, 'mouseMove', x, y);
      }
      return;
  }
}

function onWheel(x: number, y: number, dx: number, dy: number) {
  const target = hit(x, y);
  if (target?.part !== 'body') return;
  target.w.win?.webContents.sendInputEvent({
    type: 'mouseWheel',
    x: Math.round(x - target.w.x),
    y: Math.round(y - target.w.y),
    deltaX: -dx,
    deltaY: -dy,
  });
}

/// C1: events from the capture window, in window coordinates.
function onSurfaceInput(ev: SurfaceEvent) {
  if (!editOn) return;
  if (ev.type === 'keydown' || ev.type === 'keyup') {
    onKey(ev.type === 'keydown', ev.key ?? '', ev.ctrl ?? false);
    return;
  }
  const [x, y] = toSwapchain(ev.x ?? 0, ev.y ?? 0, ev.vw, ev.vh);
  if (ev.type === 'wheel') onWheel(x, y, ev.dx ?? 0, ev.dy ?? 0);
  else onPointer(ev.type, x, y, ev.button);
}

/// X keycode (XKB, evdev + 8) → DOM key name for keys without text.
const KEY_NAMES: Record<number, string> = {
  9: 'Escape', 22: 'Backspace', 23: 'Tab', 36: 'Enter', 37: 'Control', 105: 'Control', 50: 'Shift', 62: 'Shift',
  110: 'Home', 111: 'Up', 113: 'Left', 114: 'Right', 115: 'End', 116: 'Down', 119: 'Delete',
};

let cursor = { x: 0, y: 0 };
let ctrlDown = false;

/// C2: input captured by the layer; coordinates already in swapchain pixels.
function onLayerInputs() {
  for (const ev of bridge.takeInputs()) {
    if (!editOn) continue;
    switch (ev.kind) {
      case 'move':
        cursor = { x: ev.x, y: ev.y };
        onPointer('move', ev.x, ev.y);
        break;
      case 'button':
        onPointer(ev.down ? 'down' : 'up', cursor.x, cursor.y, ev.button);
        break;
      case 'wheel':
        onWheel(cursor.x, cursor.y, ev.dx, ev.dy);
        break;
      case 'key': {
        if (ev.code === 37 || ev.code === 105) ctrlDown = ev.down;
        const name = KEY_NAMES[ev.code] ?? ev.text ?? '';
        if (name) onKey(ev.down, name, ctrlDown, ev.text ?? undefined);
        break;
      }
    }
  }
}

/// Win32 virtual-key codes (from the C3 hook) → DOM key names for keys without printable text.
const WIN_VK_NAMES: Record<number, string> = {
  0x1b: 'Escape', 0x08: 'Backspace', 0x09: 'Tab', 0x0d: 'Enter',
  0x11: 'Control', 0xa2: 'Control', 0xa3: 'Control', 0x10: 'Shift', 0xa0: 'Shift', 0xa1: 'Shift',
  0x24: 'Home', 0x23: 'End', 0x25: 'Left', 0x26: 'Up', 0x27: 'Right', 0x28: 'Down', 0x2e: 'Delete',
};

/// Printable char for a Win32 VK code. A spike-level ASCII mapping (letters/digits/space) — enough
/// for P3's typing test; a release would resolve text through the hook's keyboard layout.
function winVkChar(vk: number, shift: boolean): string | undefined {
  if (vk >= 0x41 && vk <= 0x5a) {
    const c = String.fromCharCode(vk);
    return shift ? c : c.toLowerCase();
  }
  if (vk >= 0x30 && vk <= 0x39) return String.fromCharCode(vk);
  if (vk === 0x20) return ' ';
  return undefined;
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/// C3: input from the in-game Win32 hook. Mouse motion is relative, so accumulate an absolute
/// cursor (clamped to the swapchain) and relay it to the layer; buttons/wheel/keys route like C2.
function onWin32Inputs() {
  const sw = bridge.swapchainSize() ?? { width: 2560, height: 1440 };
  for (const ev of bridge.takeInputs()) {
    if (!editOn) continue;
    switch (ev.kind) {
      case 'move':
        winCursor.x = clamp(winCursor.x + ev.x, 0, sw.width - 1);
        winCursor.y = clamp(winCursor.y + ev.y, 0, sw.height - 1);
        bridge.setCursor(winCursor.x, winCursor.y);
        onPointer('move', winCursor.x, winCursor.y);
        break;
      case 'button':
        onPointer(ev.down ? 'down' : 'up', winCursor.x, winCursor.y, ev.button);
        break;
      case 'wheel':
        onWheel(winCursor.x, winCursor.y, ev.dx, ev.dy);
        break;
      case 'key': {
        const vk = ev.code;
        if (vk === 0x11 || vk === 0xa2 || vk === 0xa3) winCtrl = ev.down;
        if (vk === 0x10 || vk === 0xa0 || vk === 0xa1) winShift = ev.down;
        const ch = winVkChar(vk, winShift);
        const name = WIN_VK_NAMES[vk] ?? ch;
        if (name) onKey(ev.down, name, winCtrl, ev.text ?? ch);
        break;
      }
    }
  }
}

// ---- Startup ---------------------------------------------------------------------------------

app.whenReady().then(() => {
  const { consumer, capture, widgets: names, gep } = parseArgs(process.argv.slice(2));
  captureMode = capture;
  bridge.startServer(SOCKET);
  if (capture === 'win32') {
    // The module installs no input hooks until told to; the Hideout runs want them.
    const plugin =
      gep === undefined
        ? undefined
        : { path: toWinePath(path.join(INSTALL_DIR, 'uw_spike_dlcollector.dll')), channel: GEP_CHANNEL, args: gep };
    bridge.startInputServer(HOST_PORT, true, plugin);
    console.log(`[osr-host] C3 input listener on 127.0.0.1:${HOST_PORT}`);
    if (plugin) console.log(`[osr-host] Spike 3: modules load ${plugin.path} (channel ${GEP_CHANNEL}, args "${gep}")`);
  } else if (gep !== undefined) {
    console.error('[osr-host] --gep needs --capture win32 (the in-game module loads the collector)');
  }
  console.log(`[osr-host] socket ${SOCKET}; capture ${capture}; widgets: ${names.join(', ')}`);
  for (const w of layout(names)) {
    widgets.push(w);
    startWidget(w, consumer);
  }
  createSurface();
  ipcMain.on('surface-input', (_e, ev: SurfaceEvent) => onSurfaceInput(ev));

  // A newly connected layer has none of our frames, and static widgets never repaint by
  // themselves: repaint everything whenever a new layer connects. Edit mode starts off.
  let generation = 0;
  setInterval(() => {
    const g = bridge.peerGeneration();
    if (g !== generation) {
      generation = g;
      if (editOn) setEdit(false, 'layer reconnected');
      if (captureMode === 'win32' && g !== 0) injectHook();
      for (const w of widgets) w.win?.webContents.invalidate();
    }
    for (const on of bridge.takeEditRequests()) setEdit(on, `hotkey (${captureMode})`);
    if (captureMode === 'win32') onWin32Inputs();
    else onLayerInputs();
  }, 8);

  setInterval(() => {
    const avg = bitmapFrames ? (bitmapMs / bitmapFrames).toFixed(3) : '-';
    console.log(`[osr-host] frames ${JSON.stringify(counts)}; toBitmap avg ${avg} ms over ${bitmapFrames}`);
  }, 5000);
  if (consumer) {
    setTimeout(() => {
      console.error('[osr-host] no acked frame within 15 s');
      app.exit(1);
    }, 15000);
  }
});

app.on('window-all-closed', () => {});
