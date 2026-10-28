// Real layer/addon saturation regression: 32 widgets, no game or input module.
import { strict as assert } from 'node:assert';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Bridge, FrameKey } from './bridge-types';
const root = resolve(__dirname, '../../..'), output = resolve(process.argv[2]); mkdirSync(output, { recursive: true });
const bridge: Bridge = require('../native/render_bridge.node');
const child = spawn(resolve(root, 'target/debug/examples/uw-render-probe'), ['--frames', '100000'], { stdio: 'pipe', env: { ...process.env, UW_LAYER_ENABLE:'1',UW_LAYER_DISABLE:'0',UW_RENDER_PROBE:'1',UW_SPIKE_LAYER_DISABLE:'1',UW_RENDER_STATS_DIR:output } });
let stderr = '', exited: number | null | undefined, started = false, report: any;
child.stderr.on('data', b => { stderr += String(b); }); child.on('close', code => { exited = code; });
const lines: string[] = []; createInterface({ input: child.stdout }).on('line', line => { lines.push(line); if (line.startsWith('REPORT ')) report = JSON.parse(line.slice(7)); });
const consumed = new Map<string, bigint>(), retired = new Set<string>();
let ready: string | undefined, publications = 0;
function pump() { if (started) for (const e of bridge.drainEvents()) {
  if (e.kind === 'ready') { assert(!ready,'unexpected reconnect'); ready = e.session; }
  if (e.kind === 'consumed') consumed.set(e.key.allocation, BigInt(e.seq));
  if (e.kind === 'retired') retired.add(e.key.allocation);
  assert(e.kind !== 'disconnected' && e.kind !== 'unavailable', `supported load disconnected: ${JSON.stringify(e)}`);
} }
const pause = (ms: number) => new Promise(r => setTimeout(r, ms));
async function wait<T>(read: () => T | undefined): Promise<T> { const end=Date.now()+7000; for (;;) { pump();const v=read();if(v!==undefined)return v;assert(exited===undefined,`probe exited ${exited}: ${stderr}`);assert(Date.now()<end,`widget load timeout: ${stderr}`);await pause(3); } }
function metrics(): any | undefined { try { return JSON.parse(readFileSync(resolve(output, `${child.pid}-resources.json`),'utf8')); } catch { return undefined; } }
function publish(key: FrameKey, seq: bigint): boolean {
  const result=bridge.publishPatch({key,seq:String(seq),x:0,y:0,width:8,height:4,stride:32,data:Buffer.alloc(128,Number(seq%256n))});
  assert(result.status!=='stale'); if(result.status==='published'){publications++;return true;}return false;
}
async function main() {
  const identity=(await wait(()=>lines.find(l=>l.startsWith('IDENTITY ')))).split(' ');
  bridge.startServer({pid:Number(identity[1]),startTicks:identity[2]});started=true;await wait(()=>ready);
  const keys:FrameKey[]=[];
  for(let i=0;i<32;i++){const key=bridge.createWidget({id:i+1,generation:1,width:8,height:4});keys.push(key);bridge.setLayout({widget:key.widget,revision:'1',x:8*(i%16),y:4*Math.floor(i/16),width:8,height:4,z:i,visible:true});}
  let seq=0n;const start=Date.now();
  while(Date.now()-start<3000){const due=Date.now()+16;pump();seq++;for(const key of keys)if(bridge.canPublish(key))publish(key,seq);await pause(Math.max(0,due-Date.now()));}
  const final=seq+1n, submitted=new Set<string>();
  await wait(()=>{for(const key of keys)if(!submitted.has(key.allocation)&&bridge.canPublish(key)&&publish(key,final))submitted.add(key.allocation);return submitted.size===32?true:undefined;});
  await wait(()=>keys.every(k=>(consumed.get(k.allocation)??0n)>=final)?true:undefined);
  assert(publications>1500,`insufficient sustained traffic ${publications}`);
  for(const key of keys)bridge.removeWidget(key.widget,'2');
  await wait(()=>retired.size===32?true:undefined);
  const plateau=await wait(()=>{const m=metrics();return m&&m.imports===0&&m.images===0&&m.descriptors===0&&m.mappings===0&&m.gpuBudget===0&&m.sharedBudget===0?m:undefined;});
  bridge.stop();started=false;child.stdin.write('quit\n');await wait(()=>report);
  assert.equal(report.status,'pass');assert.equal(report.validationErrors,0);assert.equal(stderr,'');
  const result={status:'pass',widgets:32,updatePeriodMs:16,updateDurationMs:3000,publications,allFinalSequencesConsumed:true,plateau,validationErrors:report.validationErrors};
  writeFileSync(resolve(output,'results.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result));
}
main().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>{if(started)bridge.stop();const timer=setTimeout(()=>child.kill(),1500);timer.unref();child.once('close',()=>clearTimeout(timer));});
