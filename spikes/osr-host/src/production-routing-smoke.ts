// Real fixture consumption in an unfocused renderer; no game/injector is started.
import { app, BrowserWindow } from 'electron';
import { strict as assert } from 'node:assert';
import * as path from 'node:path';
import { ProductionRouting, applyRoutingSurface, invalidateRoutingSurface } from './production-routing';
app.disableHardwareAcceleration();
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, width: 850, height: 560, webPreferences: { offscreen: true } });
  const timeout = setTimeout(() => { console.error('routing renderer smoke timeout'); app.exit(1); }, 10000);
  await win.loadFile(path.join(__dirname, '..', 'widgets', 'production-routing.html'));
  const commands: any[] = [], failures: string[] = [];
  const receiver = new ProductionRouting(async (event, application) => {
    if (application.current()) await win.webContents.executeJavaScript(applyRoutingSurface(event, application.generation), false);
  }, command => commands.push(command), reason => failures.push(reason), generation => {
    void win.webContents.executeJavaScript(invalidateRoutingSurface(generation), false);
  });
  const consume = (event: any) => receiver.consume(JSON.stringify(event));
  consume({ event: 'routing_state', state: { epoch: '9007199254740993', revision: '0', width: 1280, height: 720,
    exclusive: false, domains: { keyboard: false, mouse: false } } });
  const config = commands[0].config;
  consume({ event: 'routing_configured', result: { epoch: config.epoch, revision: config.revision, request_id: config.request_id, code: 'applied', through_seq: '0' } });
  const r = (id: number, kind: number, a: number, flags = 0, b = 0) => ({ revision: config.revision,
    target: { id, generation: 1 }, kind: 'input', argument: 0, input: { kind, a, flags, b, c: 0, d: 0, modifiers: 0 } });
  const records = [r(1, 2, 97), r(2, 2, 0x1f600), ...[0, 1, 2, 3, 4].map(button => r(2, 4, button, 1)), r(1, 5, 120, 0, -120)];
  consume({ event: 'routed_batch', batch: { epoch: config.epoch, first_seq: '1', events: records } });
  consume({ event: 'routing_challenge', challenge: { epoch: config.epoch, nonce: '1', through_seq: '8' } });
  while (!commands.some(command => command.cmd === 'routing_ack')) await new Promise(resolve => setTimeout(resolve, 5));
  const displayed = await win.webContents.executeJavaScript(`Array.from(document.querySelectorAll('article')).map(node=>({id:node.dataset.target,text:node.querySelector('.text').textContent,buttons:node.querySelector('.buttons').textContent,wheel:node.querySelector('.wheel').textContent}))`);
  assert.deepEqual(displayed.map((value: any) => [value.id, value.text]), [['1:1', 'Text: a'], ['2:1', 'Text: 😀']]);
  assert.equal(displayed[1].buttons, 'Buttons: left, middle, right, back, forward');
  assert.equal(displayed[0].wheel, 'Wheel: 120, -120');
  assert.equal(win.isFocused(), false);
  assert.equal(commands.at(-1).ack.epoch, '9007199254740993');
  await win.webContents.executeJavaScript('window.underwolfPauseTarget=2');
  consume({ event: 'routed_batch', batch: { epoch: config.epoch, first_seq: '9', events: [r(2, 2, 120)] } });
  consume({ event: 'routing_challenge', challenge: { epoch: config.epoch, nonce: '2', through_seq: '9' } });
  while (!await win.webContents.executeJavaScript('window.underwolfPending()')) await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(commands.filter(command => command.cmd === 'routing_ack').length, 1);
  consume({ event: 'overlay_disconnected' });
  await win.webContents.executeJavaScript('window.underwolfResume()');
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(await win.webContents.executeJavaScript('window.underwolfMutations.some(event=>event.kind===2&&event.value===120)'), false);
  assert.equal(commands.filter(command => command.cmd === 'routing_ack').length, 1);
  assert.deepEqual(failures, []);
  clearTimeout(timeout);
  console.log('PASS routing fixture: two targets, Unicode once, five buttons, both wheels, awaited consumption, stale async invalidation, no focus');
  win.destroy(); app.exit(0);
}).catch(error => { console.error(error); app.exit(1); });
