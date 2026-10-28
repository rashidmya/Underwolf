import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { Readiness, type RenderTarget } from './readiness';
const session = '1a'.repeat(16), widget = { id: 1, generation: 1 };
const key = { session, widget, allocation: '1' };
function fixture() {
  let now = 1_000_000_000n; const changes: boolean[] = [];
  const r = new Readiness(() => now, ready => changes.push(ready));
  const target: RenderTarget = { widget, visible: true, key, baselineSeq: '9007199254740993' };
  r.setInput({ connected: true, epoch: '9007199254740993', width: 1000, height: 800 });
  r.setTargets([target]); r.onBridge({ kind: 'ready', session });
  r.onBridge({ kind: 'swapchain', generation: '1', width: 1000, height: 800, ready: true });
  const consumed = () => r.onBridge({ kind: 'consumed', key, seq: '9007199254740993' });
  const progress = (draw = '1') => r.onBridge({ kind: 'progress', swapchainGeneration: '1', completedDraw: draw, monotonicNs: String(now) });
  return { r, target, changes, consumed, progress, advance: (ns: bigint) => { now += ns; r.tick(); }, now: () => now };
}
test('a socket or Registered cannot enable visibility; all baselines and a later completed draw can', () => {
  const f = fixture(); f.r.onBridge({ kind: 'registered', key }); f.progress(); assert(!f.r.ready);
  f.consumed(); assert(!f.r.ready); f.advance(1n); f.progress('2'); assert(f.r.ready); assert.equal(f.changes.at(-1), true);
});
test('progress expiry releases readiness and replayed progress or registration cannot freshen it', () => {
  const f = fixture(); f.consumed(); f.advance(1n); f.progress(); assert(f.r.ready);
  const stamp = f.now(); f.advance(500_000_001n); assert(!f.r.ready);
  f.r.onBridge({ kind: 'registered', key });
  f.r.onBridge({ kind: 'progress', swapchainGeneration: '1', completedDraw: '1', monotonicNs: String(stamp) }); assert(!f.r.ready);
  f.progress('2'); assert(f.r.ready);
});
test('a pending paint over 500ms releases even when other widgets still produce progress', () => {
  const f = fixture(); f.consumed(); f.advance(1n); f.progress(); assert(f.r.ready);
  f.target.pendingSince = f.now(); f.r.setTargets([f.target]); f.advance(500_000_001n); f.progress('2'); assert(!f.r.ready);
  f.target.pendingSince = undefined; f.r.setTargets([f.target]); assert(f.r.ready);
});
test('reconnect and same-size allocation replacement require new consumption and later draw', () => {
  const f = fixture(); f.consumed(); f.advance(1n); f.progress(); assert(f.r.ready);
  f.r.onBridge({ kind: 'disconnected', reason: 'test' }); assert(!f.r.ready);
  const next = { ...key, session: '2b'.repeat(16), allocation: '2' };
  f.r.onBridge({ kind: 'ready', session: next.session }); f.target.key = next; f.r.setTargets([f.target]);
  f.r.onBridge({ kind: 'swapchain', generation: '1', width: 1000, height: 800, ready: true });
  f.consumed(); f.progress('2'); assert(!f.r.ready);
  f.r.onBridge({ kind: 'consumed', key: next, seq: '9007199254740993' }); f.advance(1n); f.progress('3'); assert(f.r.ready);
  f.target.key = { ...next, allocation: '3' }; f.r.setTargets([f.target]); assert(!f.r.ready);
});
test('new swapchain and two visible targets share one physical layout and independent baselines', () => {
  const f = fixture(); const second = { ...key, widget: { id: 2, generation: 1 }, allocation: '2' };
  f.r.setTargets([f.target, { widget: second.widget, visible: true, key: second, baselineSeq: '2' }]);
  f.consumed(); f.advance(1n); f.progress(); assert(!f.r.ready);
  f.r.onBridge({ kind: 'consumed', key: second, seq: '2' }); f.advance(1n); f.progress('2'); assert(f.r.ready);
  f.r.onBridge({ kind: 'swapchain', generation: '2', width: 900, height: 700, ready: true }); assert(!f.r.ready);
  f.progress('3'); assert(!f.r.ready); // old swapchain progress
  f.r.setInput({ connected: true, epoch: '9007199254740993', width: 900, height: 700 });
  f.r.onBridge({ kind: 'progress', swapchainGeneration: '2', completedDraw: '3', monotonicNs: String(f.now()) }); assert(f.r.ready);
});
