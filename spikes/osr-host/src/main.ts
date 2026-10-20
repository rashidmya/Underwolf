// Electron offscreen-rendering host for the overlay spike.
//
// Renders each widget in a hidden offscreen BrowserWindow with shared textures and hands every
// frame (a dmabuf) to the Vulkan layer through the native bridge. Every frame is released after
// the bridge answers (acked / dropped / timeout), so Chromium's small frame pool never drains.
//
// Must be launched with: --ozone-platform=x11 --disable-gpu-sandbox --use-angle=vulkan
// --enable-features=Vulkan (see package.json). On the NVIDIA proprietary driver, Chromium's GL
// path can't render into GBM buffers and delivers no frames; ANGLE-on-Vulkan can.
//
//   npm start -- [--consumer] [--widget solid|animated|static]...

import { app, BrowserWindow } from 'electron';
import * as os from 'node:os';
import * as path from 'node:path';

type SendResult = 'acked' | 'dropped' | 'timeout';

interface Bridge {
  startServer(socketPath: string): void;
  sendFrame(f: {
    widget: number;
    x: number;
    y: number;
    width: number;
    height: number;
    pixelFormat: string;
    modifier: string;
    planes: { fd: number; stride: number; offset: number; size: number }[];
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
const warned = new Set<string>();
function warnOnce(msg: string) {
  if (!warned.has(msg)) {
    warned.add(msg);
    console.warn(`[osr-host] ${msg}`);
  }
}

function startWidget(w: Widget, consumer: boolean) {
  const win = new BrowserWindow({
    show: false,
    width: w.width,
    height: w.height,
    transparent: true,
    backgroundColor: '#00000000',
    webPreferences: { offscreen: { useSharedTexture: true } },
  });
  win.webContents.setFrameRate(60);
  win.webContents.on('paint', async (e) => {
    const tex = e.texture;
    if (!tex) {
      warnOnce('paint event without a shared texture');
      return;
    }
    try {
      const info = tex.textureInfo;
      const pixmap = info.handle?.nativePixmap;
      if (!pixmap) {
        warnOnce('shared texture has no nativePixmap handle (Spike 2 blocker)');
        return;
      }
      if (info.pixelFormat !== 'bgra' && info.pixelFormat !== 'rgba') {
        warnOnce(`unsupported pixel format '${info.pixelFormat}' (Spike 2 blocker)`);
        return;
      }
      const result = await bridge.sendFrame({
        widget: w.id,
        x: w.x,
        y: w.y,
        width: info.codedSize.width,
        height: info.codedSize.height,
        pixelFormat: info.pixelFormat,
        modifier: pixmap.modifier,
        planes: pixmap.planes.map((p) => ({ fd: p.fd, stride: p.stride, offset: p.offset, size: p.size })),
      });
      counts[result]++;
      if (consumer && result === 'acked') app.quit();
    } finally {
      tex.release();
    }
  });
  win.loadFile(path.join(__dirname, '..', 'widgets', `${w.name}.html`));
}

app.whenReady().then(() => {
  const { consumer, widgets } = parseArgs(process.argv.slice(2));
  bridge.startServer(SOCKET);
  console.log(`[osr-host] socket ${SOCKET}; widgets: ${widgets.join(', ')}`);
  for (const w of layout(widgets)) startWidget(w, consumer);
  setInterval(() => console.log(`[osr-host] frames ${JSON.stringify(counts)}`), 5000);
  if (consumer) {
    setTimeout(() => {
      console.error('[osr-host] no acked frame within 15 s');
      app.exit(1);
    }, 15000);
  }
});

app.on('window-all-closed', () => {});
