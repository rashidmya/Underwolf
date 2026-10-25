// Test-only targeted fixture consumption. The injected router alone chooses targets.
export type WidgetRef = { id: number; generation: number };
export type WidgetSpec = { instance: WidgetRef; x: number; y: number; width: number; height: number; z: number;
  mode: 'block' | 'pass' | 'notify'; visible: boolean; keyboard: boolean };
type InputRecord = { kind: number; flags: number; a: number; b: number; c: number; d: number; modifiers: number };
type RoutedRecord = { revision: string; target: WidgetRef | null; kind: 'input' | 'hotkey' | 'cancel' | 'focus' | 'leave'; argument: number; input: InputRecord | null };
type State = { epoch: string; revision: string; width: number; height: number; exclusive: boolean; domains: { keyboard: boolean; mouse: boolean } };
type Hotkey = { id: number; vk: number; modifiers: number; flags: number; action: string; target: WidgetRef | null };
export type RoutingConfig = { epoch: string; revision: string; request_id: number; width: number; height: number; widgets: WidgetSpec[]; hotkeys: Hotkey[] };
export type RoutingSurfaceEvent = { type: 'state'; state: State } | { type: 'table'; config: RoutingConfig }
  | { type: 'record'; record: RoutedRecord; widget: WidgetSpec | null } | { type: 'reset' };
export type RoutingApplication = { generation: number; current: () => boolean };
export const ROUTING_SURFACE = { width: 850, height: 560 } as const;
export function validateFixtureBounds(widgets: WidgetSpec[]): void {
  for (const widget of widgets) {
    if (widget.visible && widget.width > 0 && widget.height > 0
      && (widget.x < 0 || widget.y < 0 || widget.x + widget.width > ROUTING_SURFACE.width || widget.y + widget.height > ROUTING_SURFACE.height)) {
      throw Error('widget exceeds fixture bitmap bounds');
    }
  }
}
type Held = { keys: Set<number>; buttons: Set<number>; focused: boolean };
type Item = { generation: number; event?: RoutingSurfaceEvent; through?: bigint; challenge?: any };
const U64_MAX = (1n << 64n) - 1n;
const u32 = (value: unknown): value is number => typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
const i32 = (value: unknown): value is number => typeof value === 'number' && Number.isInteger(value) && value >= -0x80000000 && value <= 0x7fffffff;
const instance = (value: any): value is WidgetRef => value && u32(value.id) && value.id > 0 && u32(value.generation) && value.generation > 0;
const key = (value: WidgetRef) => `${value.id}:${value.generation}`;
function counter(value: unknown, zero = true): bigint {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) throw Error('invalid decimal counter');
  const parsed = BigInt(value);
  if (parsed > U64_MAX || !zero && parsed === 0n) throw Error('counter outside u64 range');
  return parsed;
}
function validInput(value: any): value is InputRecord {
  if (!value || ![value.a, value.b, value.c, value.d].every(i32) || !u32(value.flags)
    || !u32(value.modifiers) || value.modifiers > 15) return false;
  switch (value.kind) {
    case 1: return value.a > 0 && value.a <= 255 && value.b >= 0 && value.b <= 65535 && value.c === 0 && value.d === 0 && value.flags <= 7;
    case 2: return value.a >= 0 && value.a <= 0x10ffff && !(value.a >= 0xd800 && value.a <= 0xdfff)
      && value.b === 0 && value.c === 0 && value.d === 0 && value.flags === 0;
    case 3: return value.c === 0 && value.d === 0 && value.flags === 0;
    case 4: return value.a >= 0 && value.a <= 4 && value.d === 0 && value.flags <= 1;
    case 5: return value.flags === 0;
    default: return false;
  }
}
function validWidgets(widgets: WidgetSpec[]): void {
  if (!Array.isArray(widgets) || widgets.length > 32) throw Error('invalid widgets');
  const ids = new Set<number>(), z = new Set<number>();
  for (const widget of widgets) {
    if (!instance(widget.instance) || ids.has(widget.instance.id) || z.has(widget.z)
      || ![widget.x, widget.y, widget.width, widget.height, widget.z].every(i32)
      || widget.width < 0 || widget.height < 0 || !['block', 'pass', 'notify'].includes(widget.mode)
      || typeof widget.visible !== 'boolean' || typeof widget.keyboard !== 'boolean') throw Error('invalid widget table');
    ids.add(widget.instance.id); z.add(widget.z);
  }
}
export function fixtureWidgets(): WidgetSpec[] {
  return [
    { instance: { id: 1, generation: 1 }, x: 100, y: 100, width: 340, height: 240, z: 1, mode: 'block', visible: true, keyboard: true },
    { instance: { id: 2, generation: 1 }, x: 360, y: 260, width: 340, height: 240, z: 2, mode: 'notify', visible: true, keyboard: true },
  ];
}
export const invalidateRoutingSurface = (generation: number) => `window.underwolfGeneration = ${generation}`;
export const applyRoutingSurface = (event: RoutingSurfaceEvent, generation: number) =>
  `if (window.underwolfGeneration === ${generation}) window.underwolfConsume(${JSON.stringify(event)}, ${generation})`;
export function routingOptions(argv: string[]): { command: string } | undefined {
  const modes = argv.flatMap((arg, i) => arg === '--capture' ? [argv[i + 1]] : []);
  if (!modes.includes('routing')) return undefined;
  if (modes.length !== 1 || argv.includes('--gep') || argv.includes('--widget')) throw Error('routing requires its own fixture and one capture mode');
  const at = argv.indexOf('--input-command');
  const command = at < 0 ? undefined : argv[at + 1];
  if (!command?.startsWith('/')) throw Error('routing requires --input-command /absolute/path/to/launcher');
  return { command };
}
export class ProductionRouting {
  readonly held = new Map<string, Held>();
  private epoch = 0n;
  private revision = 0n;
  private received = 0n;
  private consumed = 0n;
  private generation = 0;
  private connected = false;
  private closed = false;
  private running = false;
  private visualActive = false;
  private requestId = 0;
  private queue: Item[] = [];
  private tables = new Map<string, RoutingConfig>();
  private pending = new Map<number, RoutingConfig>();
  private current?: RoutingConfig;
  private desired?: RoutingConfig;
  private state?: State;
  constructor(private apply: (event: RoutingSurfaceEvent, application: RoutingApplication) => Promise<void>,
    private send: (command: any) => void, private fail: (reason: string) => void,
    private invalidate: (generation: number) => void = () => {}, private captureVisual: (on: boolean) => void = () => {}) {}
  get capturing(): boolean { return this.connected && !this.closed && !!this.state?.exclusive; }
  get widgets(): WidgetSpec[] { return structuredClone(this.desired?.widgets ?? fixtureWidgets()); }
  private id(): number {
    if (this.requestId === 0xffffffff) throw Error('request ids exhausted');
    return ++this.requestId;
  }
  private visual(on: boolean): void {
    if (this.visualActive !== on) { this.visualActive = on; this.captureVisual(on); }
  }
  private reset(): void {
    this.generation++; this.invalidate(this.generation); this.queue = []; this.held.clear();
    this.received = this.consumed = 0n; this.pending.clear(); this.tables.clear(); this.current = undefined;
  }
  private close(reason: string): void {
    if (this.closed) return;
    this.closed = true; this.connected = false; this.visual(false); this.reset(); this.fail(reason);
  }
  configure(widgets: WidgetSpec[], toggle?: { vk: number; modifiers: number }): RoutingConfig | undefined {
    if (!this.connected || this.closed || !this.state) return;
    try {
      validWidgets(widgets);
      validateFixtureBounds(widgets);
      if (this.pending.size >= 32) throw Error('configuration queue full');
      if (this.revision === U64_MAX) throw Error('revision exhausted');
      const request_id = this.id();
      const binding = toggle ?? this.desired?.hotkeys[0] ?? { vk: 9, modifiers: 8 };
      // Ctrl+F10 is a separate named fixture control, so a rebind must remain disjoint.
      if (!u32(binding.vk) || binding.vk < 1 || binding.vk > 255 || [1, 2, 4, 5, 6, 16, 17, 18, 91, 92, 160, 161, 162, 163, 164, 165].includes(binding.vk)
        || !u32(binding.modifiers) || binding.modifiers > 0xfff || binding.vk === 0x79 && binding.modifiers === 8) throw Error('invalid or conflicting toggle');
      const config: RoutingConfig = { epoch: String(this.epoch), revision: String(++this.revision), request_id,
        width: this.state.width, height: this.state.height, widgets: structuredClone(widgets), hotkeys: [
          { id: 1, ...binding, flags: 1, action: 'toggle_edit', target: null },
          { id: 2, vk: 0x79, modifiers: 8, flags: 1, action: 'notify', target: null },
        ] };
      this.desired = config; this.tables.set(config.revision, config); this.pending.set(request_id, config);
      this.send({ cmd: 'configure_input', request_id, config });
      return config;
    } catch (error) { this.close(String(error)); }
  }
  request(action: 'enter_edit' | 'leave_edit' | 'takeover' | 'release_takeover', target: WidgetRef | null = null): void {
    if (!this.connected || this.closed || !this.state || !this.current) return;
    try {
      const request_id = this.id();
      this.send({ cmd: 'route_request', request_id, request: { epoch: String(this.epoch), revision: this.state.revision,
        request_id, target, action, state_seq: null } });
    } catch (error) { this.close(String(error)); }
  }
  consume(line: string): void {
    if (this.closed) return;
    try {
      if (Buffer.byteLength(line, 'utf8') > 32768) throw Error('routing line too large');
      const event = JSON.parse(line);
      if (['overlay_disconnected', 'game_exited', 'stopped'].includes(event.event)) {
        this.connected = false; this.visual(false); this.reset();
        if (event.event !== 'overlay_disconnected') { this.epoch = 0n; this.desired = undefined; }
        this.enqueue({ event: { type: 'reset' } }); return;
      }
      if (event.event === 'routing_state') {
        const state = event.state, epoch = counter(state?.epoch, false), revision = counter(state?.revision);
        if (!u32(state.width) || !u32(state.height) || state.width > 0x7fffffff || state.height > 0x7fffffff
          || typeof state.exclusive !== 'boolean' || typeof state.domains?.keyboard !== 'boolean' || typeof state.domains?.mouse !== 'boolean') throw Error('invalid routing state');
        if (epoch < this.epoch || !this.connected && epoch === this.epoch) return;
        if (epoch > this.epoch) {
          this.reset();
          this.enqueue({ event: { type: 'reset' } });
        }
        this.connected = true; this.epoch = epoch; this.revision = this.revision > revision ? this.revision : revision;
        this.state = state; this.visual(state.exclusive); this.enqueue({ event: { type: 'state', state } });
        if (!this.desired && state.width && state.height) this.configure(fixtureWidgets());
        return;
      }
      if (event.event === 'routed_batch') {
        const batch = event.batch, epoch = counter(batch?.epoch, false);
        if (!this.connected || epoch < this.epoch) return;
        const first = counter(batch.first_seq, false);
        if (epoch !== this.epoch || first !== this.received + 1n || !Array.isArray(batch.events)
          || batch.events.length < 1 || batch.events.length > 64 || first + BigInt(batch.events.length) - 1n > U64_MAX) throw Error('invalid routing sequence');
        const items: Item[] = batch.events.map((record: RoutedRecord, index: number) => {
          counter(record.revision, false);
          const table = this.tables.get(record.revision);
          const widget = record.target && table?.widgets.find(value => value.instance.id === record.target!.id && value.instance.generation === record.target!.generation);
          if (!u32(record.argument) || record.target !== null && !instance(record.target)
            || record.kind !== 'hotkey' && !widget) throw Error('unknown routing target generation or revision');
          switch (record.kind) {
            case 'input': if (!validInput(record.input) || record.argument !== 0) throw Error('invalid input'); break;
            case 'hotkey': if (record.target !== null || record.argument === 0 || record.input !== null) throw Error('invalid hotkey'); break;
            case 'cancel': if (record.argument > 10 || record.input !== null) throw Error('invalid cancel'); break;
            case 'focus': if (record.argument > 1 || record.input !== null) throw Error('invalid focus'); break;
            case 'leave': if (record.argument !== 0 || record.input !== null) throw Error('invalid leave'); break;
            default: throw Error('invalid routed kind');
          }
          return { generation: this.generation, event: { type: 'record', record, widget: widget ?? null }, through: first + BigInt(index) };
        });
        this.received = first + BigInt(items.length) - 1n;
        for (const item of items) this.enqueue(item);
        return;
      }
      if (event.event === 'routing_configured' || event.event === 'routing_result') {
        const result = event.result, epoch = counter(result?.epoch, false), revision = counter(result?.revision, false), through = counter(result?.through_seq);
        if (!this.connected || epoch < this.epoch) return;
        if (epoch !== this.epoch || through > this.received || !u32(result.request_id) || result.request_id === 0) throw Error('invalid application result');
        if (event.event === 'routing_configured') {
          let config = this.pending.get(result.request_id);
          if (config && (config.epoch !== result.epoch || config.revision !== result.revision)) throw Error('configuration correlation mismatch');
          this.pending.delete(result.request_id);
          if (result.code !== 'applied') { this.tables.delete(result.revision); return; }
          if (!config) {
            // uw-host republishes only our desired table, with fresh epoch/revision/id.
            if (!this.desired || revision < this.revision) return;
            config = { ...structuredClone(this.desired), epoch: result.epoch, revision: result.revision, request_id: result.request_id };
          }
          this.revision = this.revision > revision ? this.revision : revision;
          this.tables.set(config.revision, config);
          this.enqueue({ event: { type: 'table', config }, through });
        }
        return;
      }
      if (event.event === 'routing_challenge') {
        const challenge = event.challenge, epoch = counter(challenge?.epoch, false), through = counter(challenge?.through_seq);
        counter(challenge.nonce, false);
        if (!this.connected || epoch < this.epoch) return;
        if (epoch !== this.epoch || through > this.received) throw Error('invalid challenge');
        this.enqueue({ challenge });
      }
    } catch (error) { this.close(String(error)); }
  }
  private enqueue(item: Omit<Item, 'generation'> | Item): void {
    const record = item.event?.type === 'record';
    if (this.queue.filter(entry => (entry.event?.type === 'record') === record).length >= (record ? 1024 : 64)) {
      this.close('routing queue overflow'); return;
    }
    this.queue.push({ ...item, generation: this.generation }); void this.drain();
  }
  private async drain(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      while (this.queue.length && !this.closed) {
        const item = this.queue.shift()!;
        const application = { generation: item.generation, current: () => !this.closed && item.generation === this.generation };
        if (item.challenge) {
          if (counter(item.challenge.through_seq) > this.consumed) throw Error('challenge before actual consumption');
          this.send({ cmd: 'routing_ack', request_id: this.id(), ack: item.challenge }); continue;
        }
        if (item.event?.type === 'table' && item.through! > this.consumed) throw Error('table before cancellation consumption');
        await this.apply(item.event!, application);
        if (!application.current()) continue;
        if (item.event?.type === 'record') {
          const { record } = item.event;
          if (record.target) {
            const id = key(record.target);
            let held = this.held.get(id);
            if (!held) { held = { keys: new Set(), buttons: new Set(), focused: false }; this.held.set(id, held); }
            if (record.kind === 'cancel') { held.keys.clear(); held.buttons.clear(); held.focused = false; }
            if (record.kind === 'focus') held.focused = record.argument === 1;
            if (record.kind === 'input' && record.input) {
              const input = record.input, set = input.kind === 1 ? held.keys : input.kind === 4 ? held.buttons : undefined;
              if (input.flags & 1) set?.add(input.a); else set?.delete(input.a);
            }
          }
          this.consumed = item.through!;
        } else if (item.event?.type === 'table') {
          this.current = item.event.config;
          const live = new Set(this.current.widgets.map(widget => key(widget.instance)));
          for (const id of this.held.keys()) if (!live.has(id)) this.held.delete(id);
          for (const [revision] of this.tables) {
            if (counter(revision) < counter(this.current.revision)) this.tables.delete(revision);
          }
        }
      }
    } catch (error) { this.close(String(error)); }
    finally { this.running = false; }
  }
}
