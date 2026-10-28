// Owned Electron windows + the real addon/CPU channel. GPU proof is a separate gate.
import { app, type NativeImage } from 'electron';
import { strict as assert } from 'node:assert';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Bridge, BridgeEvent } from './bridge-types';
import { createSurface, type Surface } from './surface';
import { InputConsumer, type SurfaceEvent, type WidgetSpec } from './input-consumer';
import type { Target } from './layout';
const bridge: Bridge = require('../native/render_bridge.node');
const root = resolve(__dirname, '../../..');
const output = resolve(root, 'target/render-4/renderer'); mkdirSync(output, { recursive: true });
app.setPath('userData', resolve(output, 'profile')); app.on('window-all-closed', () => {});
const pause = (): Promise<void> => new Promise(r => setTimeout(r, 5));
async function wait<T>(read: () => T | undefined | Promise<T | undefined>): Promise<T> { const until = Date.now() + 4000; for (;;) { const r = await read(); if (r !== undefined) return r; assert(Date.now() < until, 'renderer/actor timeout'); await pause(); } }
let child: ChildProcessWithoutNullStreams | undefined;
let lines: string[] = [];
async function actor(): Promise<BridgeEvent & { kind: 'ready' }> {
  child = spawn(resolve(root, 'target/debug/examples/addon_consumer'), [], { stdio: 'pipe' }); lines = [];
  child.stderr.on('data', b => process.stderr.write(b));
  createInterface({ input: child.stdout }).on('line', l => lines.push(l));
  const identity = (await line('IDENTITY')).split(' ');
  bridge.startServer({ pid: Number(identity[1]), startTicks: identity[2] }); await line('CONNECTED');
  return await wait(() => bridge.drainEvents().find((e): e is BridgeEvent & { kind: 'ready' } => e.kind === 'ready'));
}
async function line(prefix: string): Promise<string> { return wait(() => { const i = lines.findIndex(l => l.startsWith(prefix)); return i < 0 ? undefined : lines.splice(i, 1)[0]; }); }
async function command(command: string, prefix: string): Promise<string> { child!.stdin.write(command + '\n'); return line(prefix); }
async function stopActor(): Promise<void> { bridge.stop(); const c = child; child = undefined; if (c) { c.stdin.write('exit\n'); c.kill(); await new Promise<void>(r => c.once('close', () => r())); } }
const epoch = '9007199254740993';
const input = (kind: number, a: number, b = 0, flags = 0) => ({ kind, a, b, c: 0, d: 0, flags, modifiers: 0 });
const target = (id: number, page: string, scale: number): Target => ({ instance: { id, generation: 1 }, x: id === 1 ? 100 : 160, y: 100, width: 240 * scale, height: 180 * scale, z: id, mode: id === 1 ? 'block' : 'notify', visible: true, keyboard: true, page });
async function run(scale: number): Promise<void> {
  const ready = await actor(); const surfaces: Surface[] = [], failures: string[] = [];
  const options = { scale, copyMode: 'dirty' as const, imageMode: 'retained-full' as const, failed: (s: string) => (failures.push(s), console.error('consumer:',s)) };
  try {
    const spec = target(1, 'small-damage', scale), surface = await createSurface(spec, bridge, options);
    assert(surface, 'drawing surface must be created'); surfaces.push(surface);
    let last: NativeImage | undefined;
    surface.win.webContents.on('paint', (_e, _r, image) => { last = image; });
    surface.producer.onBridgeEvent(ready);
    const session = ready.session;
    await command('register', 'REGISTERED');
    await wait(() => { surface.producer.flush(); return surface.producer.counters.lastPublished > 0n ? true : undefined; });
    await command('claim', 'CLAIMED');
    await surface.win.webContents.executeJavaScript('window.drawStep(1)');
    await wait(() => last?.toBitmap()[2] === 255 ? true : undefined);
    await surface.win.webContents.executeJavaScript('window.drawStep(2)');
    await wait(() => last?.toBitmap().subarray(-4).equals(Buffer.from([0,255,0,255])) ? true : undefined);
    surface.win.webContents.stopPainting();
    const paints = surface.producer.counters.paintCount;
    await command('complete', 'COMPLETED');
    await wait(() => { surface.producer.flush(); return surface.producer.pendingSnapshots() === 0 ? true : undefined; });
    assert.equal((await command('claim', 'CLAIMED')).split(' ')[2], '2040'); await command('complete', 'COMPLETED');
    assert.equal(surface.producer.counters.paintCount, paints);
    surface.producer.onBridgeEvent({ kind: 'disconnected', reason: 'owned restart' });
    await stopActor(); const fresh = await actor();
    surface.producer.onBridgeEvent(fresh);
    await command('register', 'REGISTERED'); await wait(() => {surface.producer.flush();return surface.producer.pendingSnapshots()===0?true:undefined;}); assert.equal((await command('claim', 'CLAIMED')).split(' ')[2], '2040'); await command('complete', 'COMPLETED');
    assert.notEqual(surface.producer.snapshot().key?.session, session);
    assert.equal(surface.producer.counters.paintCount, paints, 'static reconnect must not require another paint');
    surface.close(); surfaces.pop(); await command('remove', 'CLEAN');

    for (const id of [1, 2]) { const s = await createSurface(target(id, 'input', scale), bridge, options); assert(s); surfaces.push(s); }
    const sent: any[] = []; let consumer!: InputConsumer;
    consumer = new InputConsumer(async (e, a) => { await Promise.all(surfaces.map(s => s.apply(e, a))); }, c => sent.push(c), s => (failures.push(s), console.error('consumer:',s)), g => surfaces.forEach(s => s.invalidate(g)), () => {}, () => ({ width: 1400, height: 1000 }));
    const consume = (e: unknown) => consumer.consume(JSON.stringify(e));
    consumer.setRenderingReady(true);
    const state = (e = epoch, exclusive = false) => ({ event: 'routing_state', state: { epoch: e, revision: '1', width: 1400, height: 1000, exclusive, domains: { keyboard: exclusive, mouse: exclusive }, takeover: null } });
    consume(state());
    const configure = (e = epoch, generation = 1) => {
      const widgets: WidgetSpec[] = [target(1, 'input', scale), { ...target(2, 'input', scale), instance: { id: 2, generation } }];
      const config = consumer.configure(widgets)!; assert(config);
      consume({ event: 'routing_configured', result: { epoch: e, revision: config.revision, request_id: config.request_id, code: 'applied', through_seq: '0' } }); return config;
    };
    let config = configure(), seq = 0;
    const records = (values: Array<[number, ReturnType<typeof input>]>, e = epoch, gen = 1) => { const first = seq + 1; seq += values.length;
      consume({ event: 'routed_batch', batch: { epoch: e, first_seq: String(first), events: values.map(([id, value]) => ({ revision: config.revision, target: { id, generation: id === 2 ? gen : 1 }, kind: 'input', argument: 0, input: value })) } }); };
    const challenge = (e = epoch) => consume({ event: 'routing_challenge', challenge: { epoch: e, nonce: epoch, through_seq: String(seq) } });
    records([[1, input(2, 0x1f600)], ...[0, 1, 2, 3, 4].map(b => [2, input(4, b, 0, 1)] as [number, ReturnType<typeof input>]), [2, input(5, 120, -120)], [2, input(3, 190, 115)]]); challenge();
    await wait(() => sent.some(c => c.cmd === 'routing_ack') ? true : undefined);
    assert.equal(sent.find(c => c.cmd === 'routing_ack').ack.epoch, epoch);
    assert.equal(await surfaces[0].win.webContents.executeJavaScript('window.underwolfModel.text'), '😀');
    assert.deepEqual(await surfaces[1].win.webContents.executeJavaScript('window.underwolfSnapshot()'), { text: '', buttons: [0, 1, 2, 3, 4], wheel: [120, -120], pointer: [30 / scale, 15 / scale] });
    await surfaces[1].win.webContents.executeJavaScript('window.underwolfPaused=true');
    records([[2, input(2, 88)]]); challenge();
    await wait(async () => await surfaces[1].win.webContents.executeJavaScript('window.underwolfPending===1') ? true : undefined);
    consumer.setRenderingReady(false); consume(state(epoch, true));
    const next = String(BigInt(epoch) + 1n); consume(state(next)); consumer.setRenderingReady(true);
    const old = surfaces.pop()!; old.close();
    const replacement = await createSurface({ ...target(2, 'input', scale), instance: { id: 2, generation: 2 } }, bridge, options); assert(replacement); surfaces.push(replacement);
    seq = 0; config = configure(next, 2); records([[2, input(2, 89)]], next, 2); challenge(next);
    try { await wait(() => sent.some(c => c.cmd === 'routing_ack' && c.ack.epoch === next) ? true : undefined); } catch (e) { console.error({sent,failures,snapshot:consumer.snapshot()});throw e; }
    assert.equal(await replacement.win.webContents.executeJavaScript('window.underwolfModel.text'), 'Y');
    assert.equal(sent.filter(c => c.cmd === 'routing_ack' && c.ack.epoch === epoch).length, 1);
    assert(!sent.some(c => c.cmd === 'route_request' && c.request.action === 'enter_edit'));
    assert(surfaces.every(s => !s.win.isFocused() && !s.win.isFocusable())); assert.deepEqual(failures, []);
  } finally { for (const s of surfaces) s.close(); await stopActor(); }
}
const timer = setTimeout(() => { child?.kill(); app.exit(1); }, 30000);
app.whenReady().then(async () => { try { for (const scale of [1, 1.5]) await run(scale);
  const report = { status: 'pass', scales: [1, 1.5], epoch, windows: 'unfocused', transport: 'real addon / CPU consumer', nativeChromiumInput: false, gpuProof: false };
  writeFileSync(resolve(output, 'results.json'), JSON.stringify(report, null, 2) + '\n'); console.log(JSON.stringify(report)); clearTimeout(timer); app.exit(0);
} catch (e) { console.error(e); clearTimeout(timer); bridge.stop(); child?.kill(); app.exit(1); } });
