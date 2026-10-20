// Electron offscreen-rendering host for the overlay spike.
//
// Renders each widget in a hidden offscreen BrowserWindow (CPU bitmap mode) and hands every frame
// to the Vulkan layer through the native bridge, via shared-memory slots. Electron's GPU
// shared-texture mode delivers empty frames on the NVIDIA driver; the owner chose this CPU copy
// instead (plan Amendment A, docs/spikes/02-shared-texture.md).
//
//   npm start -- [--consumer] [--widget solid|animated|static]...

import { app, BrowserWindow } from 'electron';
import * as os from 'node:os';
import * as path from 'node:path';

type SendResult = 'acked' | 'dropped' | 'timeout';

interface Bridge {
  startServer(socketPath: string): void;
  peerGeneration(): number;
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

const SIZES: Record<string, { width: number; height: number }> = {
  solid: { width: 400, height: 300 },
  animated: { width: 400, height: 300 },
  static: { width: 300, height: 200 },
};

interface Widget {
  id: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
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

/// Hard-coded layout: widgets stacked down the left side, starting at (100, 100).
function layout(names: string[]): Widget[] {
  let y = 100;
  return names.map((name, id) => {
    const size = SIZES[name];
    if (!size) throw new Error(`unknown widget '${name}' (known: ${Object.keys(SIZES).join(', ')})`);
    const w = { id, name, x: 100, y, ...size };
    y += size.height + 50;
    return w;
  });
}

const counts: Record<SendResult, number> = { acked: 0, dropped: 0, timeout: 0 };
let bitmapMs = 0;
let bitmapFrames = 0;
const windows: BrowserWindow[] = [];

function startWidget(w: Widget, consumer: boolean) {
  const win = new BrowserWindow({
    show: false,
    width: w.width,
    height: w.height,
    transparent: true,
    backgroundColor: '#00000000',
    webPreferences: { offscreen: true },
  });
  windows.push(win);
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

app.whenReady().then(() => {
  const { consumer, widgets } = parseArgs(process.argv.slice(2));
  bridge.startServer(SOCKET);
  console.log(`[osr-host] socket ${SOCKET}; widgets: ${widgets.join(', ')}`);
  for (const w of layout(widgets)) startWidget(w, consumer);
  // A newly connected layer has none of our frames, and static widgets never repaint by
  // themselves: repaint everything whenever a new layer connects.
  let generation = 0;
  setInterval(() => {
    const g = bridge.peerGeneration();
    if (g !== generation) {
      generation = g;
      for (const win of windows) win.webContents.invalidate();
    }
  }, 250);
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
