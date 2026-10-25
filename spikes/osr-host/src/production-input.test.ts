import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { ProductionInput, productionOptions, BUTTONS, setCaptureDimming } from './production-input';
const state = (epoch = 1) => JSON.stringify({ event: 'input_state', state: { epoch, active: true, width: 800, height: 600 } });
const batch = (records: unknown[], first_seq = 1) => JSON.stringify({ event: 'input_batch', batch: { epoch: 1, first_seq, records } });
const key = { kind: 1, flags: 1, a: 65, b: 30, c: 0, d: 0, modifiers: 0 };
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
test('production mode excludes old capture and requires explicit command', () => {
  assert.deepEqual(productionOptions(['--capture', 'production', '--input-command', '/tmp/launch']), { command: '/tmp/launch' });
  assert.throws(() => productionOptions(['--capture', 'production', '--capture', 'win32', '--input-command', '/tmp/launch']));
  assert.throws(() => productionOptions(['--capture', 'production']));
});
test('key down does not synthesize a character; epoch clears held state', async () => {
  const applied: any[] = [];
  const r = new ProductionInput(async e => { applied.push(e); }, () => {}, reason => assert.fail(reason));
  r.consume(state()); await tick();
  r.consume(batch([key])); await tick();
  assert.deepEqual([...r.heldKeys], [65]);
  assert.equal(applied.flatMap(e => e.records ?? []).filter(e => e.kind === 2).length, 0);
  r.consume(state(2)); await tick();
  assert.deepEqual([...r.heldKeys], []);
  assert.deepEqual(BUTTONS, ['left', 'middle', 'right', 'back', 'forward']);
});
test('challenge waits for surface consumption, including idle challenges', async () => {
  let resume: (() => void) | undefined;
  const sent: any[] = [];
  const r = new ProductionInput(async () => new Promise<void>(resolve => { resume = resolve; }), e => { sent.push(e); }, reason => assert.fail(reason));
  r.consume(state());
  r.consume(JSON.stringify({ event: 'input_challenge', challenge: { epoch: 1, nonce: 1, through_seq: 0 } }));
  assert.equal(sent.length, 0);
  resume!(); await tick();
  assert.equal(sent.length, 0);
  resume!(); await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].cmd, 'input_ack');
});
test('overflow and unsafe integer progress close the receiver', () => {
  let failed = 0;
  const r = new ProductionInput(() => new Promise(() => {}), () => {}, () => { failed++; });
  r.consume(state());
  for (let i = 1; i <= 66; i++) r.consume(batch([key], i));
  assert.equal(failed, 1);
  const q = new ProductionInput(async () => {}, () => {}, () => { failed++; });
  q.consume(state(Number.MAX_SAFE_INTEGER + 1));
  assert.equal(failed, 2);
});
test('disconnect invalidates queued idle challenges and same-epoch state', async () => {
  const sent: unknown[] = [];
  const r = new ProductionInput(async () => {}, e => sent.push(e), reason => assert.fail(reason));
  r.consume(state()); await tick();
  r.consume('{"event":"overlay_disconnected"}');
  r.consume(state());
  r.consume('{"event":"input_challenge","challenge":{"epoch":1,"nonce":2,"through_seq":0}}');
  await tick();
  assert.equal(sent.length, 0);
});

test('in-flight paused application is cancelled by a newer epoch', async () => {
  let resume!: () => void;
  const applied: any[] = [];
  const sent: unknown[] = [];
  const r = new ProductionInput(async (event, application) => {
    if (event.type === 'records') await new Promise<void>(resolve => { resume = resolve; });
    if (application.current()) applied.push(event);
  }, command => sent.push(command), reason => assert.fail(reason));
  r.consume(state()); await tick();
  r.consume(batch([{ ...key, kind: 2, a: 120 }]));
  r.consume(state(2));
  resume(); await tick();
  assert.equal(applied.some(e => e.type === 'records'), false);
  assert.equal(applied.at(-1).state.epoch, 2);
  assert.equal(sent.length, 0);
});

test('dimming follows accepted capture state even while renderer consumption is paused', () => {
  const dim: boolean[] = [];
  const r = new ProductionInput(() => new Promise(() => {}), () => {}, reason => assert.fail(reason), () => {}, on => dim.push(on));
  r.consume(state());
  assert.deepEqual(dim, [true]);
  assert.equal(r.capturing, true);
  r.consume(JSON.stringify({ event: 'input_state', state: { epoch: 2, active: false, width: 800, height: 600 } }));
  assert.deepEqual(dim, [true, false]);
  r.consume(state());
  assert.equal(r.capturing, false);
  assert.deepEqual(dim, [true, false], 'stale capture must not restore dimming');
  r.consume(state(3));
  assert.equal(r.capturing, true);
  r.consume('{"event":"overlay_disconnected"}');
  assert.equal(r.capturing, false);
  assert.deepEqual(dim, [true, false, true, false]);
});

test('drawing-only dimming parks the decorative cursor before enabling the layer', () => {
  const messages: unknown[] = [];
  const bridge = { setCursor: (x: number, y: number) => { messages.push(['cursor', x, y]); return true; },
    setEditMode: (on: boolean) => { messages.push(['dim', on]); return true; } };
  setCaptureDimming(bridge, true); setCaptureDimming(bridge, false);
  assert.deepEqual(messages, [['cursor', -64, -64], ['dim', true], ['dim', false]]);
});
