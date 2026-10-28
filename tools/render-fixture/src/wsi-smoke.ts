// Owned WSI/IPC probe. Pixel readback uses the companion offscreen production-
// source harness; this test exercises the compiled layer and real Node addon.
import { strict as assert } from 'node:assert';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdirSync, readFileSync, writeFileSync, readdirSync, appendFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Bridge, FrameKey, WidgetKey } from './bridge-types';
const root = resolve(__dirname, '../../..');
const output = resolve(process.argv[2] ?? resolve(root, 'target/render-4/wsi'));
mkdirSync(output, { recursive: true });
const bridge: Bridge = require('../native/render_bridge.node');
const child = spawn(resolve(root, 'target/debug/examples/uw-render-probe'), ['--frames', '100000'], {
  env: { ...process.env, UW_LAYER_ENABLE: '1', UW_LAYER_DISABLE: '0', UW_SPIKE_LAYER_DISABLE: '1', UW_RENDER_PROBE: '1', UW_RENDER_STATS_DIR: output },
  stdio: ['pipe', 'pipe', 'pipe'],
});
writeFileSync(resolve(output, 'wsi.stderr'), '');
let stderr = '', exited: number | null | undefined, started = false;
child.stderr.on('data', bytes => { stderr += String(bytes); appendFileSync(resolve(output, 'wsi.stderr'), bytes); });
child.on('exit', code => { exited = code; });
const lines: string[] = []; createInterface({ input: child.stdout }).on('line', line => { lines.push(line); appendFileSync(resolve(output, 'wsi.stdout'), line + '\n'); });
let session: string | undefined, swapGeneration = 0n, completedDraw = 0n;
const consumed = new Map<string, string>(), retired = new Set<string>();
let totalBytes = 0n, revision = 0n, reconnects = 0;
const base = 9007199254740993n;
const tag = (key: FrameKey): string => `${key.session}:${key.allocation}`;
function pump(): void {
  if (!started) return;
  for (const event of bridge.drainEvents()) {
    if (event.kind === 'ready') { session = event.session; consumed.clear(); retired.clear(); }
    else if (event.kind === 'consumed') consumed.set(tag(event.key), event.seq);
    else if (event.kind === 'retired') retired.add(tag(event.key));
    else if (event.kind === 'swapchain' && event.ready) swapGeneration = BigInt(event.generation);
    else if (event.kind === 'progress' && BigInt(event.swapchainGeneration) === swapGeneration) completedDraw = BigInt(event.completedDraw);
    else if (event.kind === 'unavailable') throw Error(event.reason);
  }
}
async function wait<T>(read: () => T | undefined, description: string, timeout = 7000): Promise<T> {
  const until = Date.now() + timeout;
  for (;;) { pump(); const value = read(); if (value !== undefined) return value;
    assert(exited === undefined, `WSI exited ${exited}: ${stderr.slice(-4000)}`);
    assert(Date.now() < until, `timeout ${description}: ${stderr.slice(-4000)}`);
    await new Promise(r => setTimeout(r, 3)); }
}
async function line(prefix: string): Promise<string> { return wait(() => { const i = lines.findIndex(l => l.startsWith(prefix)); return i < 0 ? undefined : lines.splice(i, 1)[0]; }, prefix); }
async function command(text: string, response: string): Promise<string> { child.stdin.write(text + '\n'); return line(response); }
type Metrics = { imports: number; images: number; descriptors: number; mappings: number; frames: number; pendingFrames: number; inFlightUploads: number; activeWidgets: number; retiredWidgets: number; sharedBudget: number; gpuBudget: number; gpuBytes: string; available: boolean };
function metrics(): Metrics | undefined {
  try { const m: Metrics = JSON.parse(readFileSync(resolve(output, `${child.pid}-resources.json`), 'utf8'));
    assert(m.available); assert(m.imports <= 128); assert(m.images <= 128); assert(m.descriptors <= 512); assert(m.frames <= 256); assert(m.sharedBudget <= 256 * 1024 * 1024); assert(m.gpuBudget <= 512 * 1024 * 1024); return m;
  } catch (e) { if ((e as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw e; }
}
async function connect(identity: string[]): Promise<void> {
  const old = session; if (started) bridge.stop(); started = false; session = undefined;
  bridge.startServer({ pid: Number(identity[1]), startTicks: identity[2] }); started = true;
  await wait(() => session, 'fresh session'); if (old) assert.notEqual(session, old); reconnects++;
}
async function publish(key: FrameKey, seq: bigint, data: Buffer, width = 16, height = 8, x = 0, y = 0): Promise<void> {
  const result = await wait(() => {
    const result = bridge.publishPatch({ key, seq: String(seq), x, y, width, height, stride: width * 4, data });
    if (result.status === 'busy') return undefined;
    assert.equal(result.status, 'published'); return result;
  }, 'nonblocking shared-mutex publication');
  assert.equal(result.seq, String(seq)); totalBytes += BigInt(result.bytes);
}
async function create(widget: WidgetKey, color: number): Promise<FrameKey> {
  const key = bridge.createWidget({ ...widget, width: 16, height: 8 });
  bridge.setLayout({ widget, revision: String(++revision), x: 2, y: 2, width: 16, height: 8, z: 1, visible: true });
  await publish(key, base, Buffer.alloc(512, color));
  await wait(() => consumed.get(tag(key)) === String(base) ? true : undefined, 'baseline GPU completion');
  return key;
}
async function main(): Promise<void> {
  const identity = (await line('IDENTITY')).split(' '); assert(child.pid); await connect(identity);
  assert(Number((await line('READY')).split(' ')[1]) >= 3);
  let key: FrameKey | undefined, fdWarm = 0, fdMax = 0;
  for (let i = 0; i < 100; i++) {
    if (i) await connect(identity);
    await command(`resize ${320 + i % 2 * 16} ${240 + i % 3 * 8}`, 'RESIZED');
    key = await create({ id: 1, generation: 1 + Math.floor(i / 10) }, i + 1);
    await publish(key, base + 1n, Buffer.from([0, 0, 0, 0]), 1, 1, 3, 2);
    await wait(() => consumed.get(tag(key!)) === String(base + 1n) ? true : undefined, 'dirty GPU completion');
    if (i % 10 === 0) { const old = key; key = await create(key.widget, i + 2); assert.notEqual(key.allocation, old.allocation); }
    const fds = readdirSync('/proc/self/fd').length; if (!i) fdWarm = fds; fdMax = Math.max(fdMax, fds);
    assert(fds <= fdWarm + 2, `host FD growth ${fds - fdWarm}`);
  }
  assert(key); await wait(() => completedDraw > 0n ? true : undefined, 'completed native draw');
  await command('pause', 'PAUSED'); await wait(() => metrics()?.pendingFrames === 0 ? true : undefined, 'drained presentation fences');
  await publish(key, base + 2n, Buffer.from([7, 8, 9, 255]), 1, 1, 0, 0);
  await wait(() => bridge.canPublish(key!) ? undefined : true, 'claimed pending patch');
  const busy = bridge.publishPatch({ key, seq: String(base + 3n), x: 0, y: 0, width: 1, height: 1, stride: 4, data: Buffer.alloc(4, 11) });
  assert.equal(busy.status, 'busy');
  const oldSession = key.session; await connect(identity);
  assert.notEqual(session, oldSession); await command('resume', 'RESUMED'); key = await create(key.widget, 201);
  await command('pause', 'PAUSED');
  bridge.removeWidget(key.widget, String(++revision));
  await wait(() => retired.has(tag(key!)) ? true : undefined, 'GPU-retired final widget');
  const final = await wait(() => { const m = metrics(); return m && m.imports === 0 && m.images === 0 && m.descriptors === 0 && m.mappings === 0 && m.pendingFrames === 0 && m.retiredWidgets === 0 && BigInt(m.gpuBytes) === totalBytes - 4n ? m : undefined; }, 'GPU/mapping plateau');
  assert.equal(final.sharedBudget, 0); assert.equal(final.gpuBudget, 0); assert.equal(BigInt(final.gpuBytes), totalBytes - 4n); assert(final.frames >= 3 && final.frames <= 8);
  child.stdin.write('quit\n'); const reportLine = await line('REPORT'); const report = JSON.parse(reportLine.slice(7)); assert.equal(report.status, 'pass'); assert.equal(report.validationErrors, 0);
  const result = { status: 'pass', generations: 100, reconnects, hostFdDelta: fdMax - fdWarm, cpuBytes: String(totalBytes), gpuBytes: final.gpuBytes, pausedDisconnect: true, abandonedCpuBytes: '4', presentationRetirement: 'observed fences', plateau: final, driver: report };
  writeFileSync(resolve(output, 'wsi.json'), JSON.stringify(result, null, 2) + '\n'); console.log(JSON.stringify(result));
}
main().catch(e => { console.error(e); process.exitCode = 1; }).finally(() => { if (started) bridge.stop(); child.kill(); });
