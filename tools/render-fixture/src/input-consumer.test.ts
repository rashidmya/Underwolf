import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { InputConsumer, localPoint, type SurfaceEvent, type Application, type WidgetSpec } from './input-consumer';
const epoch = '9007199254740993';
const tick = (): Promise<void> => new Promise(r => setImmediate(r));
const widgets = (): WidgetSpec[] => [
  { instance: { id: 1, generation: 1 }, x: 100, y: 100, width: 340, height: 240, z: 1, mode: 'block', visible: true, keyboard: true },
  { instance: { id: 2, generation: 1 }, x: 300, y: 240, width: 300, height: 200, z: 2, mode: 'notify', visible: true, keyboard: true },
];
function state(e = epoch, exclusive = false) { return { event: 'routing_state', state: { epoch: e, revision: '1', width: 1000, height: 800, exclusive, domains: { keyboard: exclusive, mouse: exclusive }, takeover: null } }; }
function fixture(apply?: (event: SurfaceEvent, application: Application) => Promise<void>) {
  const sent: any[] = [], applied: SurfaceEvent[] = [], failures: string[] = [], visuals: boolean[] = [];
  const receiver = new InputConsumer(async (e, a) => { if (apply) await apply(e, a); if (a.current()) applied.push(e); }, c => sent.push(c), s => failures.push(s), () => {}, on => visuals.push(on), () => ({ width: 1000, height: 800 }));
  const consume = (e: unknown): void => receiver.consume(JSON.stringify(e));
  receiver.setRenderingReady(true); consume(state());
  const configure = (table = widgets()): any => { const c = receiver.configure(table); assert(c, 'connected input must configure'); consume({ event: 'routing_configured', result: { epoch: c.epoch, revision: c.revision, request_id: c.request_id, code: 'applied', through_seq: '0' } }); return c; };
  const config = configure();
  const batch = (events: any[], seq = '1', e = epoch): void => consume({ event: 'routed_batch', batch: { epoch: e, first_seq: seq, events } });
  const challenge = (through = '0', e = epoch): void => consume({ event: 'routing_challenge', challenge: { epoch: e, nonce: '9007199254740993', through_seq: through } });
  return { receiver, consume, configure, config, batch, challenge, sent, applied, failures, visuals };
}
const input = (kind: number, a: number, b = 0, flags = 0) => ({ kind, a, b, c: 0, d: 0, flags, modifiers: 0 });
const record = (config: any, id: number, value: any) => ({ revision: config.revision, target: { id, generation: 1 }, kind: 'input', argument: 0, input: value });
test('exact epochs, Unicode, five buttons and both wheels are acknowledged only after target application', async () => {
  const f = fixture(); const events = [record(f.config, 1, input(2, 0x1f600)), ...[0, 1, 2, 3, 4].map(b => record(f.config, 2, input(4, b, 0, 1))), record(f.config, 2, input(5, 120, -120))];
  f.batch(events); f.challenge(String(events.length)); await tick();
  assert.deepEqual(f.failures, []); assert.equal(f.applied.filter(e => e.type === 'record').length, 7); assert.equal(f.receiver.held.get('2:1')?.buttons.size, 5);
  const ack = f.sent.find(c => c.cmd === 'routing_ack'); assert.equal(ack.ack.epoch, epoch); assert.equal(ack.ack.through_seq, '7');
});
test('render loss cancels queued mutations, sends real release/hide and withholds old-epoch challenges across recovery', async () => {
  let resume!: () => void; const blocked = new Promise<void>(r => { resume = r; }); const mutations: string[] = [];
  const f = fixture(async (e, a) => { if (e.type === 'record') { await blocked; if (a.current()) mutations.push('old'); } });
  f.batch([record(f.config, 1, input(1, 65, 0, 1))]); f.challenge('1'); await tick();
  f.receiver.setRenderingReady(false); f.consume(state(epoch, true)); resume(); await tick();
  assert.equal(f.receiver.capturing, false); assert.deepEqual(mutations, []); assert(f.sent.some(c => c.cmd === 'route_request' && c.request.action === 'leave_edit'));
  assert(f.sent.some(c => c.cmd === 'configure_input' && c.config.widgets.every((w: WidgetSpec) => !w.visible)));
  assert.equal(f.sent.filter(c => c.cmd === 'routing_ack').length, 0);
  f.receiver.setRenderingReady(true); assert(f.configure().widgets.every((w: WidgetSpec) => !w.visible)); f.challenge('1'); await tick(); assert.equal(f.sent.filter(c => c.cmd === 'routing_ack').length, 0);
  const next = String(BigInt(epoch) + 1n); f.consume(state(next)); f.configure(); f.challenge('0', next); await tick();
  assert.equal(f.sent.filter(c => c.cmd === 'route_request' && c.request.action === 'enter_edit').length, 0);
  assert.equal(f.sent.at(-1).ack.epoch, next);
});
test('physical layouts support all routing policies and convert to logical coordinates once', async () => {
  const f = fixture();
  for (const mode of ['block', 'pass', 'notify'] as const) { const table = widgets(); table[1].mode = mode; assert.equal(f.configure(table).widgets[1].mode, mode); }
  for (const scale of [1, 1.5]) assert.deepEqual(localPoint(widgets()[1], 330, 255, scale), { x: 30 / scale, y: 15 / scale });
  const outside = widgets(); outside[1].x = 999; assert.equal(f.receiver.configure(outside), undefined); assert.match(f.failures.at(-1)!, /bounds/);
});
test('noncanonical native counters close the input session', () => {
  for (const bad of ['-1', '01', '18446744073709551616']) { const f = fixture(); f.consume(state(bad)); assert.match(f.failures.at(-1)!, /counter|range/); }
});
