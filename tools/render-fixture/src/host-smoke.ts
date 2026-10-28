// Real production main + layer + owned WSI process. Input peer is a protocol fixture.
import { strict as assert } from 'node:assert';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
const root = resolve(__dirname, '../../..'), output = resolve(process.argv[2]); mkdirSync(output, { recursive: true });
const children: ChildProcessWithoutNullStreams[] = [];
function child(command: string, args: string[], env = process.env) { const c = spawn(command, args, { cwd: root, env, stdio: 'pipe' }); children.push(c); c.stderr.on('data', b => process.stderr.write(b)); return c; }
async function wait<T>(read: () => T | undefined): Promise<T> { const end = Date.now() + 10000; for (;;) { const value = read(); if (value !== undefined) return value; assert(Date.now() < end, 'production host smoke timeout'); await new Promise(r => setTimeout(r, 10)); } }
const probe = child(resolve(root, 'target/debug/examples/uw-render-probe'), ['--frames', '100000', '--width', '1000', '--height', '800'], { ...process.env, UW_LAYER_ENABLE: '1', UW_LAYER_DISABLE: '0', UW_RENDER_PROBE: '1', UW_RENDER_STATS_DIR: resolve(output, 'native'), UW_RENDER_TRACE_DIR: resolve(output, 'trace') });
const lines: string[] = []; createInterface({ input: probe.stdout }).on('line', l => lines.push(l));
function line(prefix: string) { return wait(() => { const i = lines.findIndex(l => l.startsWith(prefix)); return i < 0 ? undefined : lines.splice(i, 1)[0]; }); }
const epoch = resolve(output, 'epoch'), commands = resolve(output, 'commands.jsonl'); writeFileSync(epoch, '9007199254740993');
const launcher = resolve(output, 'input-peer');
writeFileSync(launcher, `#!${process.execPath}\nprocess.stdout.on('error',()=>process.exit(0));process.stdin.on('end',()=>process.exit(0));const fs=require('node:fs'),rl=require('node:readline');let epoch='',nonce=1n;const emit=v=>console.log(JSON.stringify(v));setInterval(()=>{const e=fs.readFileSync(${JSON.stringify(epoch)},'utf8');if(e!==epoch){epoch=e;emit({event:'routing_state',state:{epoch,revision:'1',width:1000,height:800,exclusive:false,domains:{keyboard:false,mouse:false},takeover:null}});}emit({event:'routing_challenge',challenge:{epoch,nonce:String(nonce++),through_seq:'0'}});},100);rl.createInterface({input:process.stdin}).on('line',line=>{fs.appendFileSync(${JSON.stringify(commands)},line+'\\n');const c=JSON.parse(line);if(c.cmd==='configure_input')emit({event:'routing_configured',result:{epoch:c.config.epoch,revision:c.config.revision,request_id:c.request_id,code:'applied',through_seq:'0'}});});`, { mode: 0o700 });
let host: ChildProcessWithoutNullStreams | undefined;
function metrics(): any | undefined { try { return JSON.parse(readFileSync(resolve(output, 'fixture/fixture.json'), 'utf8')); } catch { return undefined; } }
function acks(): number { try { return readFileSync(commands, 'utf8').split('\n').filter(l => l && JSON.parse(l).cmd === 'routing_ack').length; } catch { return 0; } }
async function main() {
  const identity = (await line('IDENTITY')).split(' '); await line('READY');
  host = child(resolve(root, 'tools/render-fixture/node_modules/.bin/electron'), ['--ozone-platform=x11', resolve(__dirname, 'main.js'), '--pid', identity[1], '--start-ticks', identity[2], '--input-command', launcher, '--scenario', 'benchmark', '--copy-mode', 'dirty', '--image-mode', 'retained-full', '--output', resolve(output, 'fixture')], { ...process.env, UW_LAYER_DISABLE: '1' });
  host.stdout.on('data', b => process.stdout.write(b));
  const ready = await wait(() => { const m = metrics(); return m?.ready ? m : undefined; });
  assert.equal(ready.widgets.length, 3); assert(ready.widgets.every((w: any) => BigInt(w.sharedBytes) > 0n)); assert.equal(ready.capture, false);
  await wait(() => acks() > 0 ? true : undefined);
  probe.stdin.write('pause\n'); await line('PAUSED');
  await wait(() => metrics()?.ready === false ? true : undefined);
  const before = acks(); await new Promise(r => setTimeout(r, 250)); assert.equal(acks(), before, 'expired actual rendering must withhold input lease acks');
  writeFileSync(epoch, '9007199254740994'); probe.stdin.write('resume\n'); await line('RESUMED');
  await wait(() => metrics()?.ready === true && acks() > before ? true : undefined); assert.equal(metrics().capture, false);
  host.stdin.write('{"cmd":"stop"}\n'); await wait(() => host!.exitCode === 0 ? true : undefined);
  probe.stdin.write('quit\n'); await wait(() => probe.exitCode === 0 ? true : undefined);
  const report = { status: 'pass', widgets: 3, actualLayerAndMain: true, expiredProgressWithholdsAcks: true, noAutoCapture: true, inputPeer: 'owned protocol fixture; production DLL/helper live acceptance remains pending' };
  writeFileSync(resolve(output, 'results.json'), JSON.stringify(report, null, 2) + '\n'); console.log(JSON.stringify(report));
}
main().catch(e => { console.error(e); process.exitCode = 1; }).finally(async () => { for (const c of children) if (c.exitCode === null) c.kill('SIGTERM'); await new Promise(r => setTimeout(r, 500)); for (const c of children) if (c.exitCode === null) c.kill('SIGKILL'); });
