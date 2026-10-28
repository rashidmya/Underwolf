#!/usr/bin/env python3
"""Owned real-addon regression for events racing synchronous session calls."""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
NODE = r'''
const assert=require('node:assert/strict'),root=process.env.UW_RECOVERY_ROOT;
const bridge=require(root+'/tools/render-fixture/native/render_bridge.node');
const {WidgetProducer}=require(root+'/tools/render-fixture/dist/producer.js');
const {ImageSource}=require(root+'/tools/render-fixture/dist/image-source.js');
const {renderCall}=require(root+'/tools/render-fixture/dist/render-session.js');
bridge.startServer({pid:Number(process.env.UW_RECOVERY_PID),startTicks:process.env.UW_RECOVERY_TICKS});
const producer=new WidgetProducer(new ImageSource('retained-full',{width:8,height:4}),bridge,{id:1,generation:1},'dirty');
let ready=false,lost=0,events=[];
const disconnected=reason=>{ready=false;lost++;producer.onBridgeEvent({kind:'disconnected',reason});};
const timeout=setTimeout(()=>{console.error('recovery smoke timed out');bridge.stop();process.exit(1);},3000);
const timer=setInterval(()=>{
 try{
  events.push(...bridge.drainEvents());
  if(!events.some(e=>e.kind==='disconnected'))return;
  clearInterval(timer);clearTimeout(timeout);
  const stale=events.find(e=>e.kind==='ready');assert(stale,'missing queued ready event');
  assert.equal(renderCall(()=>producer.onBridgeEvent(stale),disconnected),false);
  for(const control of [
   ()=>bridge.setLayout({widget:{id:1,generation:1},revision:'1',x:0,y:0,width:8,height:4,z:0,visible:true}),
   ()=>bridge.setDimming({revision:'1',active:false}),
   ()=>bridge.removeWidget({id:1,generation:1},'1')])assert.equal(renderCall(control,disconnected),false);
  assert.equal(lost,4);assert.equal(ready,false);assert.equal(producer.snapshot().key,undefined);
  producer.close();bridge.stop();console.log(JSON.stringify({status:'pass',realAddon:true,staleOperations:['create','layout','dimming','remove'],leaseAcknowledgements:0}));
 }catch(e){console.error(e);clearInterval(timer);clearTimeout(timeout);bridge.stop();process.exitCode=1;}
},5);
console.log('STARTED');
'''


def main():
    pid = os.getpid()
    ticks = Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()[19]
    with tempfile.TemporaryDirectory(prefix='uw-render-recovery-') as home:
        env = dict(os.environ, HOME=home, UW_RECOVERY_ROOT=str(ROOT), UW_RECOVERY_PID=str(pid), UW_RECOVERY_TICKS=ticks)
        child = subprocess.Popen(['node', '-e', NODE], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            assert child.stdout.readline().strip() == 'STARTED'
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
                peer.settimeout(2)
                peer.connect(f'{home}/.cache/underwolf/render/{pid}-{ticks}.sock')
                payload = struct.pack('<IQQHHII', pid, int(ticks), 4096, 40, 8, 1, 1)
                peer.sendall(struct.pack('<4sHHIHH', b'UWFR', 1, 1, len(payload), 0, 0) + payload)
                header = b''
                while len(header) < 16:
                    data = peer.recv(16-len(header))
                    assert data, 'missing Welcome header'
                    header += data
                remaining = struct.unpack('<4sHHIHH', header)[3]
                while remaining:
                    data = peer.recv(remaining)
                    assert data, 'truncated Welcome'
                    remaining -= len(data)
            out, err = child.communicate(timeout=5)
            assert child.returncode == 0, err + out
            result = json.loads(out.strip())
            assert result['status'] == 'pass'
            print(json.dumps(result))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()


if __name__ == '__main__':
    main()
