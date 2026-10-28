import { app } from 'electron';
import { mkdirSync, writeFileSync, renameSync } from 'node:fs';
import { resolve, isAbsolute } from 'node:path';
import { createInterface } from 'node:readline';
import type { Bridge, BridgeEvent } from './bridge-types';
import { InputConsumer, type WidgetSpec } from './input-consumer';
import { InputSession } from './input-session';
import { Readiness } from './readiness';
import { layout, type Scenario, type Target } from './layout';
import { createSurface, type Surface } from './surface';
import type { ImageMode } from './image-source';
import { renderCall } from './render-session';

const usage = '--pid <pid> --start-ticks <u64> --input-command <absolute launcher> --copy-mode dirty|full --image-mode retained-full|owned-backing --scenario benchmark|input|small-damage --output <dir>';
if (process.argv.includes('--help')) { console.log(usage); app.exit(0); }
const args = new Map<string, string>();
// Electron retains its switches before the application entry in process.argv.
const appIndex = process.argv.findIndex((arg, i) => i > 0 && resolve(arg) === __filename);
if (appIndex < 0) throw Error(usage);
for (let i = appIndex + 1; i < process.argv.length; i += 2) { const key = process.argv[i], value = process.argv[i + 1]; if (!key.startsWith('--') || !value || args.has(key)) throw Error(usage); args.set(key, value); }
if ([...args.keys()].some(k => !['--pid', '--start-ticks', '--input-command', '--copy-mode', '--image-mode', '--scenario', '--output'].includes(k))) throw Error(usage);
const pid = Number(args.get('--pid')), startTicks = args.get('--start-ticks') ?? '';
const command = args.get('--input-command') ?? '', copyMode = args.get('--copy-mode') ?? 'dirty', imageMode = args.get('--image-mode') ?? 'retained-full', scenario = args.get('--scenario') ?? 'benchmark';
if (!Number.isInteger(pid) || pid <= 0 || !/^[1-9][0-9]*$/.test(startTicks) || BigInt(startTicks) > (1n << 64n) - 1n
  || !isAbsolute(command) || !['dirty', 'full'].includes(copyMode) || !['retained-full', 'owned-backing'].includes(imageMode) || !['benchmark', 'input', 'small-damage'].includes(scenario) || !args.get('--output')) throw Error(usage);
const output = resolve(args.get('--output')!); mkdirSync(output, { recursive: true });
app.setPath('userData', resolve(output, 'profile')); app.on('window-all-closed', () => {});
const bridge: Bridge = require('../native/render_bridge.node');
let input: InputSession | undefined, consumer: InputConsumer, readiness: Readiness;
let surfaces: Surface[] = [], targets: Target[] = [], bounds = { width: 0, height: 0 };
let connected = false, closing = false, failed = false, revision = 0n, dimming = false;
let poll: ReturnType<typeof setInterval> | undefined, metrics: ReturnType<typeof setInterval> | undefined;
const recent: Array<{ at: string; event: unknown }> = [];
const now = (): bigint => BigInt(bridge.monotonicNowNs());
function record(event: unknown): void { recent.push({ at: String(now()), event }); if (recent.length > 128) recent.shift(); }
function configure(): void { consumer.configure(targets.map(({ page, ...w }) => w)); }
function renderLost(reason: string): void {
  connected = false; dimming = false;
  const event: BridgeEvent = { kind: 'disconnected', reason }; record(event);
  for (const s of surfaces) s.producer.onBridgeEvent(event);
  readiness.onBridge(event);
}
function sendLayouts(): boolean {
  if (!connected) return false;
  for (const [i, s] of surfaces.entries()) {
    if (s.producer.snapshot().key) {
      const t = targets[i];
      if (!renderCall(() => bridge.setLayout({ widget: t.instance, revision: String(++revision), x: t.x, y: t.y, width: t.width, height: t.height, z: t.z, visible: t.visible }), renderLost)) return false;
    }
  }
  return true;
}
function writeMetrics(): void {
  const at = now();
  const data = { schemaVersion: 1, status: failed ? 'fail' : closing ? 'stopped' : 'running', pid, startTicks, copyMode, imageMode, scenario,
    electron: process.versions.electron, monotonicNs: String(at), ready: readiness?.ready ?? false, connected,
    capture: consumer?.capturing ?? false, bounds, native: bridge.stats(),
    widgets: surfaces.map((s, i) => ({ instance: targets[i].instance, ...s.producer.counters, ...s.producer.snapshot(),
      pendingAgeNs: s.producer.snapshot().pendingSince === undefined ? null : String(at - s.producer.snapshot().pendingSince!) })), recent };
  const dest = resolve(output, 'fixture.json'); writeFileSync(dest + '.tmp', JSON.stringify(data, (_k, v) => typeof v === 'bigint' ? String(v) : v, 2) + '\n'); renameSync(dest + '.tmp', dest);
}
function shutdown(reason?: string): void {
  if (closing) return; closing = true; failed = !!reason;
  if (reason) console.error(reason);
  if (poll) clearInterval(poll); if (metrics) clearInterval(metrics);
  consumer?.setRenderingReady(false); input?.stop();
  try { record({ stopped: reason ?? 'operator' }); writeMetrics(); } catch (e) { console.error(e); failed = true; }
  for (const s of surfaces) s.close(); bridge.stop(); app.exit(failed ? 1 : 0);
}
function refresh(): void {
  readiness.setTargets(targets.map((t, i) => ({ widget: t.instance, visible: t.visible, ...surfaces[i].producer.snapshot() })));
  const snapshot = consumer.snapshot(), state = snapshot.state;
  readiness.setInput({ connected: snapshot.connected, epoch: state?.epoch ?? '0', width: state?.width ?? 0, height: state?.height ?? 0 }); readiness.tick();
}
function onBridge(event: BridgeEvent): void {
  record(event);
  if (event.kind === 'ready') { connected = true; dimming = false; }
  if (event.kind === 'disconnected' || event.kind === 'unavailable') { renderLost(event.reason); return; }
  if (event.kind === 'swapchain') {
    bounds = { width: event.width, height: event.height };
    const modes = targets.map(t => t.mode); targets = layout(scenario as Scenario, bounds); targets.forEach((t, i) => { t.mode = modes[i] ?? t.mode; });
  }
  for (const s of surfaces) if (!renderCall(() => s.producer.onBridgeEvent(event), renderLost)) return;
  if ((event.kind === 'ready' || event.kind === 'need_baseline' || event.kind === 'swapchain') && !sendLayouts()) return;
  readiness.onBridge(event);
  if (event.kind === 'ready') renderCall(() => bridge.setDimming({ revision: String(++revision), active: false }), renderLost);
}
app.whenReady().then(async () => {
  targets = layout(scenario as Scenario, bounds);
  for (const t of targets) surfaces.push(await createSurface(t, bridge, { scale: 1, copyMode: copyMode as 'dirty' | 'full', imageMode: imageMode as ImageMode, animateSparse: scenario === 'small-damage', failed: shutdown }));
  consumer = new InputConsumer(async (event, application) => {
    await Promise.all(surfaces.map(s => s.apply(event, application)));
    if (application.current() && event.type === 'record' && event.record.kind === 'hotkey' && event.record.argument === 2 && targets[1]) {
      const modes: WidgetSpec['mode'][] = ['block', 'pass', 'notify']; targets[1].mode = modes[(modes.indexOf(targets[1].mode) + 1) % 3]; configure();
    }
  }, c => input?.send(c), shutdown, g => surfaces.forEach(s => s.invalidate(g)), on => {
    const active = on && connected;
    if (active !== dimming) { dimming = active; if (connected) renderCall(() => bridge.setDimming({ revision: String(++revision), active }), renderLost); }
  }, () => bounds);
  readiness = new Readiness(now, ready => { record({ readiness: ready }); consumer.setRenderingReady(ready); configure(); });
  bridge.startServer({ pid, startTicks });
  input = new InputSession(command, line => { consumer.consume(line); refresh(); }, shutdown, { ...process.env, UW_INPUT_PID: String(pid), UW_INPUT_START_TICKS: startTicks });
  poll = setInterval(() => { try { for (const e of bridge.drainEvents()) onBridge(e); for (const s of surfaces) s.producer.flush(); refresh(); } catch (e) { shutdown(String(e)); } }, 10);
  metrics = setInterval(() => { try { writeMetrics(); } catch (e) { shutdown(String(e)); } }, 500);
  // Operator controls use the same production requests as the physical hotkeys.
  createInterface({ input: process.stdin }).on('line', line => {
    try {
      if (Buffer.byteLength(line) > 32768) throw Error('operator line too long');
      const c = JSON.parse(line);
      if (c.cmd === 'stop') shutdown();
      else if (['enter_edit', 'leave_edit', 'takeover', 'release_takeover'].includes(c.cmd)) consumer.request(c.cmd, c.target ?? null);
      else if (c.cmd === 'mode' && targets[1] && ['block', 'pass', 'notify'].includes(c.mode)) { targets[1].mode = c.mode; configure(); }
      else throw Error('unknown operator command');
    } catch (e) { console.error(String(e)); }
  });
  console.log(JSON.stringify({ status: 'started', pid, startTicks, copyMode, imageMode, scenario, output }));
}).catch(e => shutdown(String(e)));
process.on('SIGINT', () => shutdown()); process.on('SIGTERM', () => shutdown());
