import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { InputSession } from './input-session';
const wait = async (read: () => boolean): Promise<void> => { const until = Date.now() + 1500; while (!read() && Date.now() < until) await new Promise(r => setTimeout(r, 5)); assert(read(), 'bounded child response missing'); };
function launcher(body: string) {
  const dir = mkdtempSync(join(tmpdir(), 'uw-input-session-'));
  const command = join(dir, 'launcher');
  writeFileSync(command, `#!${process.execPath}\n${body}`, { mode: 0o700 });
  return { command, clean: () => rmSync(dir, { recursive: true, force: true }) };
}
test('owned launcher exchanges exact JSONL and UTF8 without a shell', async () => {
  const f = launcher(`process.stdin.setEncoding('utf8'); let b='';process.stdin.on('data',s=>{b+=s;if(b.includes('\\n')){const v=Buffer.from(b);process.stdout.write(v.subarray(0,v.length-3));setTimeout(()=>process.stdout.write(v.subarray(v.length-3)),5);b='';}});`);
  const lines: string[] = [], failures: string[] = [];
  const session = new InputSession(f.command, s => lines.push(s), s => failures.push(s));
  try { session.send({ epoch: '9007199254740993', text: '😀' }); await wait(() => lines.length === 1);
    assert.deepEqual(JSON.parse(lines[0]), { epoch: '9007199254740993', text: '😀' }); assert.deepEqual(failures, []);
  } finally { session.stop(); f.clean(); }
});
test('overlong or unterminated child output fails closed once', async () => {
  for (const body of [`process.stdout.write('x'.repeat(32769));setInterval(()=>{},1000);`, `process.stdout.write('{');`]) {
    const f = launcher(body), failures: string[] = [];
    const session = new InputSession(f.command, () => assert.fail('partial output dispatched'), s => failures.push(s));
    try { await wait(() => failures.length > 0); await new Promise(r => setTimeout(r, 20)); assert.equal(failures.length, 1); }
    finally { session.stop(); f.clean(); }
  }
});
test('launcher must be an explicit absolute executable', () => {
  assert.throws(() => new InputSession('uw-inject', () => {}, () => {}), /absolute/);
});
