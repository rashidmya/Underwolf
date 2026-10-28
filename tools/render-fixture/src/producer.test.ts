import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import type { NativeImage } from 'electron';
import type { Bridge, FrameKey, PublishResult } from './bridge-types';
import { ImageSource, type RectLike, type PixelPatch } from './image-source';
import { WidgetProducer } from './producer';
const full = { x: 0, y: 0, width: 8, height: 4 };
const a = { x: 0, y: 0, width: 1, height: 1 };
const b = { x: 7, y: 3, width: 1, height: 1 };
const widget = { id: 1, generation: 1 };
function image(bytes: Buffer, width = 8, height = 4): NativeImage {
  return { getSize: () => ({ width, height }), toBitmap: () => Buffer.from(bytes),
    crop: (r: RectLike) => { const patch = Buffer.alloc(r.width * r.height * 4);
      for (let row = 0; row < r.height; row++) bytes.copy(patch, row * r.width * 4,
        ((r.y + row) * width + r.x) * 4, ((r.y + row) * width + r.x + r.width) * 4);
      return image(patch, r.width, r.height); } } as NativeImage;
}
class FakeBridge {
  now = 100n;
  monotonicNowNs(): string { return String(this.now); }
  busy = false; race = false; generation = 0; session = '1a'.repeat(16);
  publications: Array<PixelPatch & { key: FrameKey; seq: string }> = [];
  pixels = Buffer.alloc(128); removed = 0;
  createWidget(): FrameKey { this.pixels.fill(0); return { session: this.session, widget, allocation: String(++this.generation) }; }
  canPublish(): boolean { return !this.busy; }
  publishPatch(p: PixelPatch & { key: FrameKey; seq: string }): PublishResult {
    if (this.race) { this.race = false; return { status: 'busy', seq: p.seq, bytes: '0' }; }
    if (this.busy) throw Error('must check ownership');
    this.publications.push({ ...p, data: Buffer.from(p.data) });
    for (let row = 0; row < p.height; row++) p.data.copy(this.pixels,
      ((p.y + row) * 8 + p.x) * 4, row * p.stride, row * p.stride + p.width * 4);
    this.busy = true;
    return { status: 'published', seq: p.seq, bytes: String(p.width * p.height * 4) };
  }
  removeWidget(): void { this.removed++; }
}
function fixture(mode: 'dirty' | 'full' = 'dirty') {
  const bridge = new FakeBridge(); const source = new ImageSource('retained-full', full);
  const producer = new WidgetProducer(source, bridge as unknown as Bridge, widget, mode);
  producer.onBridgeEvent({ kind: 'ready', session: bridge.session });
  const bytes = Buffer.alloc(128, 7); producer.onPaint(image(bytes), full);
  return { bridge, producer, bytes };
}
for (const mode of ['dirty', 'full'] as const) {
  test(`${mode}: disjoint paints remain bounded and flush after completion without another paint`, () => {
    const { bridge, producer, bytes } = fixture(mode);
    assert.equal(bridge.publications.length, 1);
    bytes.fill(9, 0, 4); producer.onPaint(image(Buffer.from(bytes)), a);
    bytes.fill(11, 124, 128); producer.onPaint(image(Buffer.from(bytes)), b);
    assert.equal(producer.pendingSnapshots(), 1); assert.equal(bridge.publications.length, 1);
    bridge.busy = false; producer.onBridgeEvent({ kind: 'consumed', key: bridge.publications[0].key, seq: '1' });
    assert.equal(bridge.publications.length, 2); assert.deepEqual(bridge.pixels, bytes);
    assert.equal(producer.pendingSnapshots(), 0); assert.equal(producer.counters.coalescedPaints, 1n);
    assert.equal(producer.counters.sharedBytes, 256n);
  });
}
test('Busy after canPublish retains damage and a plain poll flush repairs a lost hint', () => {
  const { bridge, producer, bytes } = fixture(); bridge.busy = false; bridge.race = true;
  bytes.fill(19, 0, 4); producer.onPaint(image(bytes), a);
  assert.equal(bridge.publications.length, 1); assert.equal(producer.pendingSnapshots(), 1);
  producer.flush(); assert.equal(bridge.publications.length, 2);
  assert.deepEqual(bridge.pixels, bytes); assert.equal(producer.counters.sharedBytes, 132n);
});
test('full mode copies full bounds even for sparse damage', () => {
  const { bridge, producer, bytes } = fixture('full'); bridge.busy = false;
  bytes.fill(19, 0, 4); producer.onPaint(image(bytes), a);
  assert.equal(bridge.publications[1].data.length, 128); assert.equal(producer.counters.sharedBytes, 256n);
});
test('static reconnect and NeedBaseline replace allocation and publish complete retained image', () => {
  const { bridge, producer, bytes } = fixture(); const old = bridge.publications[0].key;
  producer.onBridgeEvent({ kind: 'disconnected', reason: 'test' });
  bridge.busy = false; bridge.session = '2b'.repeat(16);
  producer.onBridgeEvent({ kind: 'ready', session: bridge.session });
  assert.deepEqual(bridge.pixels, bytes); assert.notEqual(bridge.publications[1].key.session, old.session);
  bridge.busy = false; producer.onBridgeEvent({ kind: 'need_baseline', widget });
  assert.deepEqual(bridge.pixels, bytes); assert.notEqual(bridge.publications[2].key.allocation, bridge.publications[1].key.allocation);
});
test('same-size replacement uses a new key and close releases retained image', () => {
  const { bridge, producer } = fixture(); bridge.busy = false;
  producer.onBridgeEvent({ kind: 'need_baseline', widget }); assert.equal(bridge.generation, 2);
  producer.close(); assert.equal(bridge.removed, 1); assert.equal(producer.pendingSnapshots(), 0);
  assert.throws(() => producer.onPaint(image(Buffer.alloc(128)), full));
});
test('readiness tracks the first baseline and oldest pending damage across Busy and reconnect', () => {
  const { bridge, producer, bytes } = fixture();
  assert.equal(producer.snapshot().baselineSeq, '1');
  assert.deepEqual(producer.snapshot().key, bridge.publications[0].key);
  bridge.now = 200n; producer.onPaint(image(bytes), a);
  bridge.now = 700n; producer.onPaint(image(bytes), b);
  assert.equal(producer.snapshot().pendingSince, 200n);
  bridge.busy = false; producer.flush();
  assert.equal(producer.snapshot().pendingSince, undefined);
  assert.equal(producer.snapshot().baselineSeq, '1');
  producer.onBridgeEvent({ kind: 'disconnected', reason: 'test' });
  assert.equal(producer.snapshot().key, undefined);
  bridge.now = 900n; bridge.busy = false; bridge.session = '3c'.repeat(16);
  producer.onBridgeEvent({ kind: 'ready', session: bridge.session });
  assert.equal(producer.snapshot().baselineSeq, '4');
  assert.equal(producer.snapshot().key?.session, bridge.session);
});
