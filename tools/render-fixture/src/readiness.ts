import type { BridgeEvent, FrameKey, WidgetKey } from './bridge-types';
export type RenderTarget = { widget: WidgetKey; visible: boolean; key?: FrameKey; baselineSeq?: string; pendingSince?: bigint };
export type InputStatus = { connected: boolean; epoch: string; width: number; height: number };
const LIMIT = 500_000_000n, MAX = (1n << 64n) - 1n;
function counter(s: string): bigint { if (!/^(0|[1-9][0-9]*)$/.test(s)) throw Error('invalid counter'); const n = BigInt(s); if (n > MAX) throw Error('counter range'); return n; }
const tag = (key: FrameKey): string => `${key.session}:${key.widget.id}:${key.widget.generation}:${key.allocation}`;
export class Readiness {
  private connected = false; private session?: string; private sessionAt = 0n;
  private swap?: { generation: string; width: number; height: number; ready: boolean };
  private input: InputStatus = { connected: false, epoch: '0', width: 0, height: 0 };
  private targets: RenderTarget[] = [];
  private consumed = new Map<string, bigint>(); private observed = new Map<string, bigint>();
  private progressAt?: bigint; private draw = 0n; private value = false;
  constructor(private readonly now: () => bigint, private readonly changed: (ready: boolean) => void) {}
  get ready(): boolean { return this.value; }
  onBridge(event: BridgeEvent): void {
    const now = this.now();
    switch (event.kind) {
      case 'ready':
        if (event.session !== this.session) { this.session = event.session; this.sessionAt = now; this.consumed.clear(); this.observed.clear(); this.progressAt = undefined; this.draw = 0n; this.swap = undefined; }
        this.connected = true; break;
      case 'disconnected': case 'unavailable': this.connected = false; this.progressAt = undefined; this.swap = undefined; break;
      case 'swapchain':
        if (!this.swap || counter(event.generation) > counter(this.swap.generation)) { this.progressAt = undefined; this.draw = 0n; }
        if (!this.swap || counter(event.generation) >= counter(this.swap.generation)) this.swap = { ...event }; break;
      case 'consumed':
        if (event.key.session === this.session) {
          const key = tag(event.key), seq = counter(event.seq);
          if (!this.consumed.has(key) && this.consumed.size === 128) { this.connected = false; break; }
          if (seq > (this.consumed.get(key) ?? 0n)) this.consumed.set(key, seq);
        } break;
      case 'retired': this.consumed.delete(tag(event.key)); this.observed.delete(tag(event.key)); break;
      case 'progress': {
        const at = counter(event.monotonicNs), draw = counter(event.completedDraw);
        if (this.connected && event.swapchainGeneration === this.swap?.generation && at >= this.sessionAt
          && at <= now && at > (this.progressAt ?? -1n) && draw > this.draw) { this.progressAt = at; this.draw = draw; }
        break;
      }
    }
    this.tick();
  }
  setTargets(targets: RenderTarget[]): void {
    if (targets.length > 32) throw Error('too many render targets');
    this.targets = structuredClone(targets);
    const live = new Set(targets.flatMap(t => t.key ? [tag(t.key)] : []));
    for (const key of this.observed.keys()) if (!live.has(key)) this.observed.delete(key);
    this.tick();
  }
  setInput(input: InputStatus): void {
    counter(input.epoch); const changed = input.connected !== this.input.connected || input.epoch !== this.input.epoch;
    this.input = { ...input }; this.tick(changed);
  }
  tick(force = false): void {
    const now = this.now(); let barrier = this.sessionAt;
    const visible = this.targets.filter(t => t.visible);
    let baselines = visible.length > 0;
    for (const target of visible) {
      if (!target.key || target.key.session !== this.session || !target.baselineSeq
        || (this.consumed.get(tag(target.key)) ?? 0n) < counter(target.baselineSeq)) { baselines = false; continue; }
      const key = tag(target.key);
      if (!this.observed.has(key)) this.observed.set(key, now);
      const observed = this.observed.get(key)!; if (observed > barrier) barrier = observed;
      if (target.pendingSince !== undefined && (target.pendingSince > now || now - target.pendingSince > LIMIT)) baselines = false;
    }
    const ready = !!(this.connected && this.input.connected && this.swap?.ready
      && this.swap.width === this.input.width && this.swap.height === this.input.height && baselines
      && this.progressAt !== undefined && this.progressAt > barrier && now >= this.progressAt && now - this.progressAt <= LIMIT);
    if (ready !== this.value || force) { this.value = ready; this.changed(ready); }
  }
}
