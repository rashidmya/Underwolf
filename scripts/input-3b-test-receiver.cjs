// Test-only receiver: identical idle drawing for v1/v2, or the targeted v2 fixture.
// No C3 input listener or collector is started. Production owns all input.
const { app, BrowserWindow } = require('electron');
const { spawn } = require('node:child_process');
const { createInterface } = require('node:readline');
const path = require('node:path');
const os = require('node:os');
const root = path.resolve(__dirname, '..');
const { ProductionInput, setCaptureDimming } = require(path.join(root, 'spikes/osr-host/dist/production-input.js'));
const { ProductionRouting, applyRoutingSurface, invalidateRoutingSurface, validateFixtureBounds } =
  require(path.join(root, 'spikes/osr-host/dist/production-routing.js'));
const bridge = require(path.join(root, 'spikes/osr-host/native/osr_bridge.node'));
const args = process.argv.slice(2);
const value = name => args[args.indexOf(name) + 1];
const mode = value('--mode');
const targeted = args.includes('--targeted');
const command = value('--input-command');
if (!['legacy', 'routing'].includes(mode) || !path.isAbsolute(command || '') ||
    targeted && mode !== 'routing' || process.env.UW_SPIKE_CAPTURE !== 'win32') {
  throw Error('requires --mode legacy|routing --input-command /absolute/launcher and UW_SPIKE_CAPTURE=win32');
}
let child, win, receiver, paused = false, resume, drawingVisible = true;
const layout = () => bridge.setLayout(1, 0, 0, drawingVisible);
const send = command => { if (!child.stdin.write(JSON.stringify(command) + '\n')) child.kill(); };
const fail = error => { console.error('[test-receiver]', error); child?.kill(); app.exit(1); };
app.whenReady().then(async () => {
  bridge.startServer(path.join(os.homedir(), '.cache/underwolf-spike/overlay.sock'));
  win = new BrowserWindow({ show: false, width: 850, height: 560, transparent: true,
    backgroundColor: '#00000000', webPreferences: { offscreen: true } });
  win.webContents.setFrameRate(60);
  win.webContents.on('paint', async (_event, _dirty, image) => {
    const { width, height } = image.getSize();
    if (!width || !height) return;
    const result = await bridge.sendBitmap({ widget: 1, x: 0, y: 0, width, height,
      stride: width * 4, pixelFormat: 'bgra', data: image.toBitmap() });
    if (result === 'acked') layout();
  });
  await win.loadFile(path.join(root, 'spikes/osr-host/widgets',
    targeted ? 'production-routing.html' : 'input-3b-idle.html'));
  const invalidate = generation => {
    void win.webContents.executeJavaScript(invalidateRoutingSurface(generation), false).catch(fail);
  };
  const apply = async (event, application) => {
    if (paused) await new Promise(resolve => { resume = resolve; });
    if (!application.current()) return;
    const code = targeted ? applyRoutingSurface(event, application.generation)
      : `if(window.underwolfGeneration===${application.generation}) window.consumeIdle(${JSON.stringify(event)})`;
    await win.webContents.executeJavaScript(code, false);
  };
  const visual = active => {
    if (!targeted && active) { fail('idle comparison entered capture'); return; }
    setCaptureDimming(bridge, active);
  };
  receiver = mode === 'routing' ? new ProductionRouting(apply, send, fail, invalidate, visual)
    : new ProductionInput(apply, send, fail, invalidate, visual);
  child = spawn(command, [], { stdio: ['pipe', 'pipe', 'inherit'] });
  console.log(`[production-input] launcher pid ${child.pid}; receiver pid ${process.pid}`);
  createInterface({ input: child.stdout }).on('line', line => {
    console.log('[production-input]', line);
    receiver.consume(line);
  });
  child.on('error', fail);
  child.on('exit', () => receiver.consume('{"event":"overlay_disconnected"}'));
  createInterface({ input: process.stdin }).on('line', line => {
    try {
      const c = JSON.parse(line);
      if (c.control === 'visible' && typeof c.on === 'boolean') {
        drawingVisible = c.on; layout(); send({ cmd: 'set_state', visible: c.on }); return;
      }
      if (c.control === 'drawing' && typeof c.on === 'boolean') {
        drawingVisible = c.on; layout(); return;
      }
      if (!(receiver instanceof ProductionRouting)) throw Error('routing controls require routing mode');
      const table = receiver.widgets, widget = table.find(w => w.instance.id === c.widget);
      if (c.control === 'request') { receiver.request(c.action, widget?.instance ?? null); return; }
      if (c.control === 'rebind') { receiver.configure(table, { vk: c.vk, modifiers: c.modifiers }); return; }
      if (c.control === 'move' && widget) { widget.x = c.x; widget.y = c.y; }
      else if (c.control === 'mode' && widget && ['block','pass','notify'].includes(c.mode)) widget.mode = c.mode;
      else throw Error('unknown test control');
      validateFixtureBounds(table); receiver.configure(table);
    } catch (error) { console.error('[test-receiver] control:', error); }
  });
  process.on('SIGUSR1', () => { paused = true; });
  process.on('SIGUSR2', () => { paused = false; resume?.(); resume = undefined; });
  let generation = 0;
  setInterval(() => {
    const next = bridge.peerGeneration();
    if (next !== generation) {
      generation = next;
      setCaptureDimming(bridge, receiver.capturing);
      win.webContents.invalidate(); layout();
    }
  }, 100);
  console.log(`[test-receiver] ${mode} ${targeted ? 'targeted' : 'matched-idle'} 850x560; receiver ready`);
}).catch(fail);
for (const sig of ['SIGTERM','SIGINT']) process.on(sig, () => { child?.kill(); app.exit(0); });
app.on('before-quit', () => child?.kill());
app.on('window-all-closed', () => {});
