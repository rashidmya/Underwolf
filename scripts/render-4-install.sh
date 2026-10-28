#!/usr/bin/env bash
# Explicit operator action. Only the production layer's owned manifest/library.
set -euo pipefail
umask 077
uw_render_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec python3 - "$uw_render_root" "$@" <<'PY'
import argparse, hashlib, json, os
from pathlib import Path
import shutil, tempfile
root = Path(os.sys.argv[1])
p = argparse.ArgumentParser(description='Install the per-run-enabled production layer, or restore its recorded predecessor.')
p.add_argument('--data-home', type=Path, default=Path(os.environ.get('XDG_DATA_HOME', str(Path.home()/'.local/share'))))
p.add_argument('--output', type=Path)
p.add_argument('--restore', type=Path)
a = p.parse_args(os.sys.argv[2:])
def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def atomic(path, data, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix='.uw-render-', dir=path.parent)
    try:
        with os.fdopen(fd,'wb') as f: f.write(data); f.flush(); os.fsync(f.fileno())
        os.chmod(tmp,mode); os.replace(tmp,path)
    finally:
        if os.path.exists(tmp): os.unlink(tmp)
if a.restore:
    record = json.loads(a.restore.read_text()); dest = Path(record['destination'])
    if dest.name != 'underwolf-render-4.json' or dest.is_symlink() or sha(dest) != record['installed_sha256']:
        raise SystemExit('owned manifest changed; refusing restoration over another installation')
    if record['previous']:
        previous = Path(record['previous'])
        if sha(previous) != record['previous_sha256']: raise SystemExit('backup changed')
        atomic(dest,previous.read_bytes(),0o644)
    else: dest.unlink()
    print('restored:',dest); raise SystemExit(0)
if a.output is None: p.error('--output is required for the installation/restoration record')
a.output = a.output.resolve(); a.output.mkdir(parents=True, exist_ok=False)
source = root/'target/release/libuw_vk_layer.so'
lib_sha = sha(source)
lib = a.data_home.resolve()/'underwolf/render-4'/lib_sha/'libuw_vk_layer.so'
dest = a.data_home.resolve()/'vulkan/implicit_layer.d/underwolf-render-4.json'
if dest.is_symlink(): raise SystemExit('refusing a symlink destination')
previous = None
if dest.exists():
    if json.loads(dest.read_text()).get('layer',{}).get('name') != 'VK_LAYER_UNDERWOLF_overlay':
        raise SystemExit('destination belongs to another layer')
    previous = a.output/'previous-manifest.json'; shutil.copy2(dest,previous)
template = json.loads((root/'crates/uw-vk-layer/underwolf_layer.json.in').read_text())
template['layer']['library_path'] = str(lib)
manifest = (json.dumps(template,indent=2)+'\n').encode()
record = {'status':'prepared','destination':str(dest),'library':str(lib),'library_sha256':lib_sha,
          'installed_sha256':hashlib.sha256(manifest).hexdigest(),'previous':str(previous) if previous else None,
          'previous_sha256':sha(previous) if previous else None,'enable':'UW_LAYER_ENABLE=1; game identity still required'}
atomic(a.output/'install.json',(json.dumps(record,indent=2)+'\n').encode())
if lib.exists():
    if sha(lib)!=lib_sha: raise SystemExit('content-addressed library changed')
else: atomic(lib,source.read_bytes(),0o755)
atomic(dest,manifest,0o644)
record['status']='installed'; atomic(a.output/'install.json',(json.dumps(record,indent=2)+'\n').encode())
print('installed:',dest);print('restore: scripts/render-4-install.sh --restore',a.output/'install.json')
PY
