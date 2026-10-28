import { strict as assert } from 'node:assert';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Bridge, BridgeEvent, FrameKey } from './bridge-types';
const bridge: Bridge = require('../native/render_bridge.node');
const root = resolve(__dirname, '../../..');
const child = spawn(resolve(root, 'target/debug/examples/addon_consumer'), [], { stdio: ['pipe', 'pipe', 'pipe'] });
const lines: string[] = []; let stderr = ''; child.stderr.on('data', b => { stderr += String(b); });
createInterface({ input: child.stdout }).on('line', line => lines.push(line));
const pause = (): Promise<void> => new Promise(r => setTimeout(r, 3));
async function wait<T>(read: () => T | undefined): Promise<T> {
  const until = Date.now() + 5000;
  for (;;) { const result = read(); if (result !== undefined) return result;
    assert(Date.now() < until, `addon timeout: ${stderr}`); await pause(); }
}
async function line(prefix: string): Promise<string> { return wait(() => { const i = lines.findIndex(s => s.startsWith(prefix)); return i < 0 ? undefined : lines.splice(i, 1)[0]; }); }
async function command(cmd: string, prefix: string): Promise<string> { child.stdin.write(cmd + '\n'); return line(prefix); }
async function event(predicate: (e: BridgeEvent) => boolean): Promise<BridgeEvent> {
  return wait(() => bridge.drainEvents().find(predicate));
}
async function main(): Promise<void> {
  const identity = (await line('IDENTITY')).split(' ');
  bridge.startServer({ pid: Number(identity[1]), startTicks: identity[2] });
  await line('CONNECTED'); await event(e => e.kind === 'ready');
  const before = readdirSync('/proc/self/fd').length;
  const widget = { id: 1, generation: 1 }; let key: FrameKey | undefined;
  for (let i = 0; i < 100; i++) {
    key = bridge.createWidget({ ...widget, width: 4, height: 2 });
    assert.equal(await command('register', 'REGISTERED'), `REGISTERED ${i + 1}`);
    const data = Buffer.alloc(32, 7); const seq = '9007199254740993';
    for (const bad of ['-1', '01', '18446744073709551616'])
      assert.throws(() => bridge.publishPatch({ key: key!, seq: bad, x: 0, y: 0, width: 4, height: 2, stride: 16, data }));
    assert.throws(() => bridge.canPublish({ ...key!, session: 'A'.repeat(32) }));
    assert.deepEqual(bridge.publishPatch({ key, seq, x: 0, y: 0, width: 4, height: 2, stride: 16, data }), { status: 'published', seq, bytes: '32' });
    data.fill(99); // The native call must have finished copying before returning.
    assert.equal(await command('claim', 'CLAIMED'), `CLAIMED ${seq} 112`);
    if (i === 0) {
      await new Promise(r => setTimeout(r, 120)); assert.equal(bridge.canPublish(key), false);
      assert.equal(bridge.publishPatch({ key, seq: '9007199254740994', x: 0, y: 0, width: 4, height: 2, stride: 16, data }).status, 'busy');
    }
    await command('complete', 'COMPLETED');
    const consumed = await event(e => e.kind === 'consumed' && e.key.allocation === key!.allocation);
    assert(consumed.kind === 'consumed'); assert.equal(consumed.seq, seq);
    await wait(() => bridge.canPublish(key!) ? true : undefined);
  }
  await command('progress', 'PROGRESS');
  const progress = await event(e => e.kind === 'progress'); assert(progress.kind === 'progress');
  assert.equal(progress.completedDraw, '9007199254740993'); assert.equal(progress.monotonicNs, '9007199254740993');
  bridge.removeWidget(widget, '1'); assert.equal(await command('remove', 'CLEAN'), 'CLEAN 0 0');
  await event(e => e.kind === 'retired' && e.key.allocation === key!.allocation);
  assert.deepEqual(bridge.stats(), { cpuBytes: '3200', publications: '100', coalescedHints: '0', livePixelBytes: '0', retiredPixelBytes: '0' });
  assert.equal(readdirSync('/proc/self/fd').length, before);
  assert(BigInt(bridge.monotonicNowNs()) > 0n);
  console.log(JSON.stringify({ status: 'pass', generations: 100, cpuBytes: bridge.stats().cpuBytes, exactSequence: '9007199254740993', consumerGpu: false }));
}
main().catch(error => { console.error(error); process.exitCode = 1; }).finally(() => { bridge.stop(); child.stdin.write('exit\n'); child.kill(); });
