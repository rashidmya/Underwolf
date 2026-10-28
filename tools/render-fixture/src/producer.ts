import type { NativeImage } from 'electron';
import type { ImageSource, RectLike } from './image-source';
import type { Bridge, BridgeEvent, FrameKey, WidgetKey } from './bridge-types';
import { renderCall } from './render-session';
const sameWidget = (a: WidgetKey, b: WidgetKey): boolean => a.id === b.id && a.generation === b.generation;
function union(a: RectLike, b: RectLike): RectLike {
  const x = Math.min(a.x, b.x), y = Math.min(a.y, b.y);
  return { x, y, width: Math.max(a.x + a.width, b.x + b.width) - x,
    height: Math.max(a.y + a.height, b.y + b.height) - y };
}
export class WidgetProducer {
  readonly counters = { paintCount: 0n, coalescedPaints: 0n, extractedBytes: 0n, extractionNs: 0n, sharedBytes: 0n, lastPublished: 0n, maxPendingNs: 0n, consumptionCount: 0n, consumptionNs: 0n, maxConsumptionNs: 0n };
  private key?: FrameKey;
  private session?: string;
  private pending?: RectLike;
  private pendingSince?: bigint;
  private baselineSeq?: string;
  private publishedAt?: bigint;
  private publishedSeq?: string;
  private hasImage = false;
  private closed = false;
  private seq = 0n;
  constructor(private readonly source: ImageSource, private readonly bridge: Bridge,
    private readonly widget: WidgetKey, private readonly mode: 'dirty' | 'full') {}
  private full(): RectLike { return { x: 0, y: 0, ...this.source.size() }; }
  private replace(): void {
    this.key = this.bridge.createWidget({ ...this.widget, ...this.source.size() });
    this.baselineSeq = undefined;
    this.publishedAt = undefined; this.publishedSeq = undefined;
    if (this.hasImage) { this.pending = this.full(); this.pendingSince ??= BigInt(this.bridge.monotonicNowNs()); this.seq++; }
  }
  onPaint(image: NativeImage, dirty: RectLike): void {
    if (this.closed) throw Error('widget producer closed');
    this.source.accept(image, dirty);
    if (!dirty.width || !dirty.height) return;
    this.counters.paintCount++; this.seq++;
    if (this.pending) this.counters.coalescedPaints++;
    const damage = !this.hasImage || this.mode === 'full' ? this.full() : dirty;
    this.hasImage = true;
    this.pendingSince ??= BigInt(this.bridge.monotonicNowNs());
    this.pending = this.pending ? union(this.pending, damage) : { ...damage };
    this.flush();
  }
  onBridgeEvent(event: BridgeEvent): void {
    if (this.closed) return;
    if (event.kind === 'consumed' && this.key && event.key.session === this.key.session
      && sameWidget(event.key.widget, this.widget) && event.key.allocation === this.key.allocation
      && event.seq === this.publishedSeq && this.publishedAt !== undefined) {
      const elapsed = BigInt(this.bridge.monotonicNowNs()) - this.publishedAt;
      this.counters.consumptionCount++; this.counters.consumptionNs += elapsed;
      if (elapsed > this.counters.maxConsumptionNs) this.counters.maxConsumptionNs = elapsed;
      this.publishedAt = undefined;
    }
    if (event.kind === 'ready' && this.session !== event.session) {
      this.session = event.session; this.replace();
    } else if (event.kind === 'need_baseline' && sameWidget(event.widget, this.widget) && this.session) {
      this.replace();
    } else if (event.kind === 'disconnected' || event.kind === 'unavailable') {
      this.key = undefined; this.session = undefined; this.baselineSeq = undefined;
      if (this.hasImage) { this.pending = this.full(); this.pendingSince ??= BigInt(this.bridge.monotonicNowNs()); }
    }
    this.flush();
  }
  flush(): void {
    if (this.pendingSince !== undefined) { const elapsed = BigInt(this.bridge.monotonicNowNs()) - this.pendingSince;
      if (elapsed > this.counters.maxPendingNs) this.counters.maxPendingNs = elapsed; }
    if (this.closed || !this.key || !this.pending || !this.hasImage || !this.bridge.canPublish(this.key)) return;
    const rect = this.pending, seq = this.seq;
    const start = process.hrtime.bigint(); const patch = this.source.extract(rect);
    this.counters.extractionNs += process.hrtime.bigint() - start;
    this.counters.extractedBytes += BigInt(patch.data.length);
    const result = this.bridge.publishPatch({ ...patch, key: this.key, seq: String(seq) });
    if (result.status === 'published') {
      this.publishedAt = BigInt(this.bridge.monotonicNowNs()); this.publishedSeq = String(seq);
      this.baselineSeq ??= String(seq);
      this.counters.sharedBytes += BigInt(result.bytes); this.counters.lastPublished = seq;
      if (this.seq === seq && this.pending === rect) { this.pending = undefined; this.pendingSince = undefined; }
    } else if (result.status === 'stale') {
      this.key = undefined; this.session = undefined; this.baselineSeq = undefined; this.pending = this.full();
    }
  }
  close(): void {
    if (this.closed) return;
    this.closed = true;
    try { if (this.key) renderCall(() => this.bridge.removeWidget(this.widget, '18446744073709551615'), () => {}); }
    finally { this.key = undefined; this.pending = undefined; this.session = undefined; this.hasImage = false; this.source.dispose(); }
  }
  pendingSnapshots(): number { return this.pending ? 1 : 0; }
  snapshot(): { key?: FrameKey; baselineSeq?: string; pendingSince?: bigint } {
    return { key: this.key && structuredClone(this.key), baselineSeq: this.baselineSeq, pendingSince: this.pendingSince };
  }
}
