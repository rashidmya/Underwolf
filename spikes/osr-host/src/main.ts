// Electron offscreen-rendering host for the overlay spikes.
//
// Renders each widget in a hidden offscreen BrowserWindow (CPU bitmap mode) and hands every frame
// to the Vulkan layer through the native bridge, via shared-memory slots. Electron's GPU
// shared-texture mode delivers empty frames on the NVIDIA driver; the owner chose this CPU copy
// instead (plan Amendment A, docs/spikes/02-shared-texture.md).
//
// Edit mode (Spike 1, candidates H1 + C1): the layer reports Ctrl+Tab; the host then raises a
// transparent always-on-top "surface" window over the game's monitor that captures all input.
// Drag a widget by the bar above it; clicks and typing inside a widget go to that widget; Ctrl+Tab,
// Escape or a click outside every widget leaves edit mode. Runs on X11 (XWayland) so the surface
// can be positioned (see package.json).
//
//   npm start -- [--consumer] [--widget solid|animated|static|input]...

import { app, BrowserWindow, ipcMain, screen } from 'electron';
import * as os from 'node:os';
import * as path from 'node:path';

type SendResult = 'acked' | 'dropped' | 'timeout';

interface Bridge {
  startServer(socketPath: string): void;
  peerGeneration(): number;
  takeEditRequests(): boolean[];
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

function parseArgs(argv: string[]): { consumer: boolean; widgets: string[] } {
  const widgets: string[] = [];
  let consumer = false;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--consumer') consumer = true;
    else if (argv[i] === '--widget' && argv[i + 1]) widgets.push(argv[++i]);
  }
  return { consumer, widgets: widgets.length ? widgets : ['animated'] };
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
let surface: BrowserWindow | undefined;
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
  if (on === editOn || !surface) return;
  editOn = on;
  bridge.setEditMode(on);
  console.log(`[osr-host] edit mode ${on ? 'ON' : 'OFF'} (${why}) at ${new Date().toISOString()}`);
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

function onSurfaceInput(ev: SurfaceEvent) {
  if (!editOn) return;
  if (ev.type === 'keydown' || ev.type === 'keyup') {
    const key = ev.key ?? '';
    if (ev.type === 'keydown' && (key === 'Escape' || (key === 'Tab' && ev.ctrl))) {
      setEdit(false, key === 'Escape' ? 'Escape' : 'Ctrl+Tab on surface');
      return;
    }
    if (!focused?.win) return;
    const wc = focused.win.webContents;
    if (ev.type === 'keydown') {
      wc.sendInputEvent({ type: 'keyDown', keyCode: key });
      if (key.length === 1) wc.sendInputEvent({ type: 'char', keyCode: key });
    } else {
      wc.sendInputEvent({ type: 'keyUp', keyCode: key });
    }
    return;
  }
  const [x, y] = toSwapchain(ev.x ?? 0, ev.y ?? 0, ev.vw, ev.vh);
  const target = hit(x, y);
  switch (ev.type) {
    case 'down':
      if (!target) {
        setEdit(false, 'click outside widgets');
        return;
      }
      if (target.part === 'bar') {
        dragging = { w: target.w, dx: x - target.w.x, dy: y - target.w.y };
      } else {
        focused = target.w;
        target.w.win?.webContents.focus();
        mouse(target.w, 'mouseDown', x, y, ev.button);
      }
      return;
    case 'up':
      if (dragging) dragging = undefined;
      else if (target?.part === 'body') mouse(target.w, 'mouseUp', x, y, ev.button);
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
    case 'wheel':
      if (target?.part === 'body') {
        target.w.win?.webContents.sendInputEvent({
          type: 'mouseWheel',
          x: Math.round(x - target.w.x),
          y: Math.round(y - target.w.y),
          deltaX: -(ev.dx ?? 0),
          deltaY: -(ev.dy ?? 0),
        });
      }
      return;
  }
}

// ---- Startup ---------------------------------------------------------------------------------

app.whenReady().then(() => {
  const { consumer, widgets: names } = parseArgs(process.argv.slice(2));
  bridge.startServer(SOCKET);
  console.log(`[osr-host] socket ${SOCKET}; widgets: ${names.join(', ')}`);
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
      for (const w of widgets) w.win?.webContents.invalidate();
    }
    for (const on of bridge.takeEditRequests()) setEdit(on, 'hotkey (layer)');
  }, 16);

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
