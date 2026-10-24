// Real offscreen renderer smoke check. No game or hook injector is started.
import { app, BrowserWindow } from 'electron';
import { strict as assert } from 'node:assert';
import * as path from 'node:path';
import { ProductionInput, applySurface, invalidateSurface } from './production-input';
app.disableHardwareAcceleration();
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, width: 640, height: 400, webPreferences: { offscreen: true } });
  const timeout = setTimeout(() => { console.error('renderer smoke timeout'); app.exit(1); }, 10000);
  await win.loadFile(path.join(__dirname, '..', 'widgets', 'production-input.html'));
  const commands: any[] = [];
  const receiver = new ProductionInput(async (event, application) => {
    if (!application.current()) return;
    await win.webContents.executeJavaScript(applySurface(event, application.generation), false);
  }, command => commands.push(command), reason => { throw Error(reason); }, generation => {
    void win.webContents.executeJavaScript(invalidateSurface(generation), false);
  });
  receiver.consume(JSON.stringify({ event: 'input_state', state: { epoch: 1, active: true, width: 800, height: 600 } }));
  const r = (kind: number, a: number, flags = 0) => ({ kind, a, flags, b: 0, c: 0, d: 0, modifiers: 0 });
  receiver.consume(JSON.stringify({ event: 'input_batch', batch: { epoch: 1, first_seq: 1, records: [r(1, 65, 1), r(2, 97), r(2, 0x1f600), ...[0, 1, 2, 3, 4].map(b => r(4, b, 1))] } }));
  receiver.consume(JSON.stringify({ event: 'input_challenge', challenge: { epoch: 1, nonce: 1, through_seq: 8 } }));
  while (!commands.length) await new Promise(resolve => setTimeout(resolve, 10));
  const displayed = await win.webContents.executeJavaScript('({text:document.getElementById("text").textContent,buttons:document.getElementById("buttons").textContent,keys:document.getElementById("keys").textContent})');
  assert.equal(displayed.text, 'Text: a😀');
  assert.equal(displayed.buttons, 'Buttons: left, middle, right, back, forward');
  assert.equal(displayed.keys, 'Keys: 65');
  assert.equal(commands[0].ack.through_seq, 8);
  assert.equal(win.isFocused(), false);
  // A stale application submitted after the renderer barrier must not mutate text.
  await win.webContents.executeJavaScript(invalidateSurface(2), false);
  await win.webContents.executeJavaScript(applySurface({ type: 'records', records: [r(2, 120)], width: 800, height: 600 }, 1), false);
  assert.equal(await win.webContents.executeJavaScript('document.getElementById("text").textContent'), 'Text: a😀');
  clearTimeout(timeout);
  console.log('PASS production surface: offscreen application, text once, five buttons, renderer-gated acknowledgement, no focus');
  win.destroy(); app.exit(0);
}).catch(error => { console.error(error); app.exit(1); });
