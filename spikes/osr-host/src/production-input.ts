// Test-surface receiver. No native Chromium input claim and no spike capture hooks.
export const BUTTONS = ['left', 'middle', 'right', 'back', 'forward'] as const;
type Record = { kind: number; flags: number; a: number; b: number; c: number; d: number; modifiers: number };
type State = { epoch: number; active: boolean; width: number; height: number };
type Challenge = { epoch: number; nonce: number; through_seq: number };
export type SurfaceEvent = { type: 'state'; state: State } | { type: 'records'; records: Record[]; width: number; height: number }
  | { type: 'challenge'; challenge: Challenge } | { type: 'reset' };
export type Application = { generation: number; current: () => boolean };
// The generation barrier and application run in the same renderer JS world.
export const invalidateSurface = (generation: number) => `window.underwolfGeneration = ${generation}`;
export const applySurface = (event: SurfaceEvent, generation: number) =>
  `if (window.underwolfGeneration === ${generation}) window.underwolfConsume(${JSON.stringify(event)})`;
type DrawingBridge = { setCursor(x: number, y: number): boolean; setEditMode(on: boolean): boolean };
/** The reused layer is in win32 drawing-only mode. Its 14px decorative cursor
 * stays outside the viewport because production capture supplies the native cursor. */
export function setCaptureDimming(bridge: DrawingBridge, on: boolean): void {
  if (on) bridge.setCursor(-64, -64);
  bridge.setEditMode(on);
}
type Item = { generation: number; event: SurfaceEvent; through?: number };
const uint = (n: unknown): n is number => typeof n === 'number' && Number.isSafeInteger(n) && n >= 0;
const positive = (n: unknown): n is number => uint(n) && n > 0;
function record(r: any): r is Record {
  return r && [r.kind, r.flags, r.a, r.b, r.c, r.d, r.modifiers].every(Number.isSafeInteger)
    && r.kind >= 1 && r.kind <= 5 && r.flags >= 0 && r.flags <= 7
    && (r.kind !== 1 || (r.a >= 0 && r.a < 256))
    && (r.kind !== 2 || (r.a >= 0 && r.a <= 0x10ffff && !(r.a >= 0xd800 && r.a <= 0xdfff)))
    && (r.kind !== 4 || (r.a >= 0 && r.a < 5));
}
export function productionOptions(argv: string[]): { command: string } | undefined {
  const modes = argv.flatMap((arg, i) => arg === '--capture' ? [argv[i + 1]] : []);
  if (!modes.includes('production')) return undefined;
  if (modes.length !== 1 || argv.includes('--gep') || argv.includes('--widget')) throw Error('production capture requires one test surface and no other capture/plugin flags');
  const at = argv.indexOf('--input-command');
  const command = at < 0 ? undefined : argv[at + 1];
  if (!command || !command.startsWith('/')) throw Error('production capture requires --input-command /absolute/path/to/launcher');
  return { command };
}
export class ProductionInput {
  readonly heldKeys = new Set<number>();
  private epoch = 0;
  private connected = false;
  private received = 0;
  private consumed = 0;
  private generation = 0;
  private state: State = { epoch: 0, active: false, width: 0, height: 0 };
  private queue: Item[] = [];
  private running = false;
  private closed = false;
  private requestId = 1;
  private visualActive = false;
  constructor(private apply: (event: SurfaceEvent, application: Application) => Promise<void>, private send: (command: unknown) => void, private fail: (reason: string) => void,
    private invalidate: (generation: number) => void = () => {}, private captureVisual: (on: boolean) => void = () => {}) {}
  get capturing(): boolean { return this.connected && !this.closed && this.state.active; }
  private visual(on: boolean): void {
    if (on === this.visualActive) return;
    this.visualActive = on; this.captureVisual(on);
  }
  private advance(): void { this.generation++; this.invalidate(this.generation); }
  consume(line: string): void {
    if (this.closed) return;
    try {
      const e = JSON.parse(line);
      if (['overlay_disconnected', 'game_exited', 'stopped'].includes(e.event)) {
        this.connected = false; this.visual(false);
        if (e.event !== 'overlay_disconnected') this.epoch = 0;
        this.advance(); this.queue = []; this.heldKeys.clear(); this.received = this.consumed = 0;
        this.enqueue({ type: 'reset' }); return;
      }
      if (e.event === 'input_state') {
        const s = e.state;
        if (!s || !positive(s.epoch) || typeof s.active !== 'boolean' || !uint(s.width) || !uint(s.height)) throw Error('invalid input state');
        if (s.epoch < this.epoch || (!this.connected && s.epoch === this.epoch)) return;
        if (s.epoch > this.epoch) {
          this.advance(); this.queue = []; this.received = this.consumed = 0; this.heldKeys.clear();
        }
        this.connected = true; this.epoch = s.epoch; this.state = s;
        this.visual(s.active);
        if (!s.active) this.heldKeys.clear();
        this.enqueue({ type: 'state', state: s });
      } else if (e.event === 'input_batch') {
        const b = e.batch;
        if (!this.connected || b?.epoch < this.epoch) return;
        if (!b || b.epoch !== this.epoch || !positive(b.first_seq) || b.first_seq !== this.received + 1
          || !Array.isArray(b.records) || b.records.length < 1 || b.records.length > 64 || !b.records.every(record)) throw Error('invalid input batch');
        const end = b.first_seq + b.records.length - 1;
        if (!uint(end)) throw Error('unsafe input sequence');
        this.received = end;
        this.enqueue({ type: 'records', records: b.records, width: this.state.width, height: this.state.height }, end);
      } else if (e.event === 'input_challenge') {
        const c = e.challenge;
        if (!this.connected || c?.epoch < this.epoch) return;
        if (!c || c.epoch !== this.epoch || !positive(c.nonce) || !uint(c.through_seq) || c.through_seq > this.received) throw Error('invalid challenge');
        this.enqueue({ type: 'challenge', challenge: c });
      }
    } catch (error) { this.close(String(error)); }
  }
  private close(reason: string): void {
    if (this.closed) return;
    this.closed = true; this.visual(false); this.advance(); this.queue = []; this.heldKeys.clear(); this.fail(reason);
  }
  private enqueue(event: SurfaceEvent, through?: number): void {
    if (this.queue.length >= 64) { this.close('input queue overflow'); return; }
    this.queue.push({ generation: this.generation, event, through });
    void this.drain();
  }
  private async drain(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      while (this.queue.length && !this.closed) {
        const item = this.queue.shift()!;
        await this.apply(item.event, { generation: item.generation, current: () => !this.closed && item.generation === this.generation });
        if (item.generation !== this.generation) continue;
        if (item.event.type === 'records') {
          for (const r of item.event.records) if (r.kind === 1) {
            if (r.flags & 1) this.heldKeys.add(r.a); else this.heldKeys.delete(r.a);
          }
          this.consumed = item.through!;
        } else if (item.event.type === 'challenge') {
          const c = item.event.challenge;
          if (c.through_seq > this.consumed) throw Error('challenge before consumption');
          this.send({ cmd: 'input_ack', request_id: this.requestId++, ack: c });
        }
      }
    } catch (error) { this.close(String(error)); }
    finally { this.running = false; }
  }
}
