import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { ProductionRouting, RoutingSurfaceEvent, RoutingApplication, fixtureWidgets, routingOptions } from './production-routing';

const tick = () => new Promise<void>(resolve => setImmediate(resolve));
const input = (kind: number, a: number, flags = 0, b = 0) => ({ kind, a, flags, b, c: 0, d: 0, modifiers: 0 });
const target = (id: number, generation = 1) => ({ id, generation });
const record = (id: number, value: any, revision = '1', generation = 1) =>
  ({ revision, target: target(id, generation), kind: 'input', argument: 0, input: value });
const state = (epoch = '1', revision = '0', exclusive = false) => ({ event: 'routing_state', state: {
  epoch, revision, width: 1000, height: 800, state_seq: null, pointer: null, focus: null, takeover: null,
  exclusive, domains: { keyboard: exclusive, mouse: exclusive }, reason: 'ready',
} });
function fixture(apply?: (event: RoutingSurfaceEvent, application: RoutingApplication) => Promise<void>, initial = state()) {
  const sent: any[] = [], applied: RoutingSurfaceEvent[] = [], failures: string[] = [], visuals: boolean[] = [];
  const receiver = new ProductionRouting(async (event, application) => {
    if (apply) await apply(event, application);
    if (application.current()) applied.push(event);
  }, command => sent.push(command), reason => failures.push(reason), () => {}, on => visuals.push(on));
  const consume = (event: any) => receiver.consume(JSON.stringify(event));
  const batch = (events: any[], first_seq = '1', epoch = '1') => consume({ event: 'routed_batch', batch: { epoch, first_seq, events } });
  const challenge = (through_seq: string, epoch = '1') => consume({ event: 'routing_challenge', challenge: { epoch, nonce: '1', through_seq } });
  const configured = (config: any, through_seq = '0') => consume({ event: 'routing_configured', result: {
    epoch: config.epoch, revision: config.revision, request_id: config.request_id, code: 'applied', through_seq,
  } });
  consume(initial);
  const config = sent.find(command => command.cmd === 'configure_input').config;
  configured(config);
  consume(state(config.epoch, config.revision));
  return { receiver, sent, applied, failures, visuals, consume, batch, challenge, configured, config };
}
test('two overlapping targets consume their own Unicode, five buttons and both wheels exactly once', async () => {
  const f = fixture();
  const events = [record(1, input(2, 97)), record(2, input(2, 0x1f600)),
    ...[0, 1, 2, 3, 4].map(button => record(2, input(4, button, 1))), record(1, input(5, 120, 0, -120))];
  f.batch(events); f.challenge(String(events.length)); await tick();
  assert.equal(f.failures.length, 0);
  const records = f.applied.filter(event => event.type === 'record');
  assert.equal(records.length, 8);
  assert.deepEqual(records.map(event => event.type === 'record' && event.record.target?.id), [1, 2, 2, 2, 2, 2, 2, 1]);
  assert.equal(f.receiver.held.get('2:1')?.buttons.size, 5);
  assert.equal(f.sent.at(-1).ack.through_seq, '8');
  const widgets = fixtureWidgets();
  assert.ok(widgets[0].x + widgets[0].width > widgets[1].x);
});
test('a frozen second target blocks progress while the first has actually consumed', async () => {
  let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  const f = fixture(async event => { if (event.type === 'record' && event.record.target?.id === 2) await blocked; });
  f.batch([record(1, input(1, 65, 1)), record(2, input(1, 66, 1))]); f.challenge('2');
  await tick();
  assert.ok(f.receiver.held.get('1:1')?.keys.has(65));
  assert.equal(f.sent.filter(command => command.cmd === 'routing_ack').length, 0);
  release(); await tick();
  assert.ok(f.receiver.held.get('2:1')?.keys.has(66));
  assert.equal(f.sent.at(-1).ack.through_seq, '2');
});
test('moving and reusing a target waits for old release/cancel before table retirement', async () => {
  let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  const f = fixture(async event => { if (event.type === 'record' && event.record.input?.flags === 1) await blocked; });
  f.batch([record(1, input(1, 65, 1)), record(1, input(1, 65))]);
  const moved = fixtureWidgets(); moved[0].x += 40; moved[0].instance.generation = 2;
  const config = f.receiver.configure(moved);
  assert.ok(config);
  f.batch([{ revision: '1', target: target(1), kind: 'cancel', argument: 1, input: null }], '3');
  f.configured(config, '3'); f.challenge('3'); await tick();
  assert.equal(f.applied.filter(event => event.type === 'table').length, 1);
  release(); await tick();
  assert.equal(f.applied.filter(event => event.type === 'table').length, 2);
  assert.equal(f.receiver.held.has('1:1'), false);
  assert.equal(f.sent.at(-1).ack.through_seq, '3');
  f.batch([record(1, input(2, 120), config!.revision, 2)], '4'); await tick();
  assert.equal(f.failures.length, 0);
  f.batch([record(1, input(2, 121), config!.revision, 1)], '5'); await tick();
  assert.match(f.failures[0], /target|generation/);
});
test('all three modes, a rebound toggle and Ctrl+F10 are published as numeric table ids', async () => {
  const f = fixture(); await tick();
  for (const mode of ['block', 'pass', 'notify'] as const) {
    const widgets = fixtureWidgets(); widgets[0].mode = mode;
    const config = f.receiver.configure(widgets, { vk: 0x79, modifiers: 1 });
    assert.equal(config?.widgets[0].mode, mode);
    assert.equal(config?.hotkeys[0].vk, 0x79);
    assert.equal(config?.hotkeys[1].vk, 0x79);
    assert.notEqual(config?.hotkeys[0].modifiers, config?.hotkeys[1].modifiers);
    assert.equal(typeof config?.widgets[0].instance.id, 'number');
    f.configured(config); await tick();
  }
  assert.equal(f.failures.length, 0);
});
test('u64 epochs and revisions retain precision and canonical decimal strings', async () => {
  const f = fixture(undefined, state('18446744073709551614', '9007199254740993')); await tick();
  const config = f.sent.filter(command => command.cmd === 'configure_input').at(-1).config;
  assert.equal(config.epoch, '18446744073709551614');
  assert.equal(config.revision, '9007199254740994');
  f.configured(config); f.challenge('0', config.epoch); await tick();
  assert.equal(f.sent.at(-1).ack.epoch, config.epoch);
  f.consume(state('01')); assert.match(f.failures[0], /counter/);
});
test('disconnect invalidates an already awaited application and cannot acknowledge it', async () => {
  let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  const mutations: string[] = [];
  const f = fixture(async (event, application) => {
    if (event.type !== 'record') return;
    await blocked;
    if (application.current()) mutations.push('stale');
  });
  f.batch([record(1, input(2, 97))]); f.challenge('1'); await tick();
  f.consume({ event: 'overlay_disconnected' }); release(); await tick();
  assert.deepEqual(mutations, []);
  assert.equal(f.sent.filter(command => command.cmd === 'routing_ack').length, 0);
  assert.equal(f.receiver.held.size, 0);
  assert.equal(f.receiver.capturing, false);
});
test('exclusive visuals follow applied state and standard mouse ownership stays undimmed', async () => {
  const f = fixture();
  f.consume(state('1', '1', true)); await tick();
  f.consume({ ...state('1', '1'), state: { ...state('1', '1').state, domains: { keyboard: false, mouse: true } } });
  await tick(); assert.deepEqual(f.visuals, [true, false]);
});
test('host epoch republishing restores only the table, and removed targets stay retired', async () => {
  const f = fixture(); await tick();
  const config = f.receiver.configure(fixtureWidgets().slice(1));
  f.configured(config); await tick();
  f.consume(state('2', config!.revision));
  const sentBefore = f.sent.length;
  f.configured({ ...config, epoch: '2', revision: String(BigInt(config!.revision) + 1n), request_id: 9000 });
  f.challenge('0', '2'); await tick();
  assert.equal(f.sent.slice(sentBefore).some(command => command.cmd === 'route_request'), false);
  assert.equal(f.sent.at(-1).ack.epoch, '2');
  f.batch([record(1, input(2, 120), String(BigInt(config!.revision) + 1n))], '1', '2');
  assert.match(f.failures[0], /target/);
});
test('routing launch excludes other capture/plugin modes and requires an explicit launcher', () => {
  assert.deepEqual(routingOptions(['--capture', 'routing', '--input-command', '/tmp/launch']), { command: '/tmp/launch' });
  assert.throws(() => routingOptions(['--capture', 'routing', '--capture', 'win32', '--input-command', '/tmp/launch']));
  assert.throws(() => routingOptions(['--capture', 'routing', '--gep', '', '--input-command', '/tmp/launch']));
  assert.throws(() => routingOptions(['--capture', 'routing']));
});
