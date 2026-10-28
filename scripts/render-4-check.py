#!/usr/bin/env python3
"""Owned GPU/WSI validation probes; never installs a layer or touches a game."""
from __future__ import annotations
import argparse
import csv
import platform
import time
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CheckFailure(RuntimeError):
    def __init__(self, message, code):
        super().__init__(message)
        self.code = code


def run(command: list[str], output: Path, name: str, env: dict[str, str], timeout: int = 60) -> str:
    process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, start_new_session=True)
    try:
        text, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            text, _ = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            text, _ = process.communicate()
        (output / f"{name}.log").write_text(text)
        raise CheckFailure(f"{name}: owned process group exceeded {timeout}s", 124)
    (output / f"{name}.log").write_text(text)
    if process.returncode:
        raise CheckFailure(f"{name} exited {process.returncode}: {text[-3000:]}", process.returncode)
    return text


def json_line(text: str) -> dict:
    for line in reversed(text.splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    raise ValueError("probe returned no JSON report")


def gpu_check(output: Path) -> int:
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, UW_SPIKE_LAYER_DISABLE="1", UW_LAYER_DISABLE="1")
    validation = ROOT / "target/render-4/validation/usr/share/vulkan/explicit_layer.d"
    layer = output / "layer"
    layer.mkdir(exist_ok=True)
    paths = [str(layer)]
    if validation.exists():
        paths.append(str(validation))
    if env.get("VK_ADD_LAYER_PATH"):
        paths.append(env["VK_ADD_LAYER_PATH"])
    env["VK_ADD_LAYER_PATH"] = ":".join(paths)
    manifest = (ROOT / "crates/uw-vk-layer/underwolf_layer.json.in").read_text()
    (layer / "underwolf_layer.json").write_text(manifest.replace(
        "@LIBRARY_PATH@", str(ROOT / "target/release/libuw_vk_layer.so")))
    report = {"schema_version": 1, "status": "running", "scope": "owned GPU/WSI probes; no live game or performance acceptance", "cases": {}}
    result_path = output / "results.json"
    try:
        run(["cargo", "build", "--release", "-p", "uw-vk-layer"], output, "build-layer", env)
        run(["cargo", "build", "-p", "uw-vk-layer", "--examples"], output, "build-probes", env)
        run(["npm", "run", "build:native", "--prefix", "tools/render-fixture"], output, "build-addon", env)
        run(["npm", "run", "build", "--prefix", "tools/render-fixture"], output, "build-fixture", env)
        for scenario in ("unorm", "srgb", "held", "churn"):
            text = run([str(ROOT / "target/debug/examples/gpu_probe"), scenario], output, scenario, env, 20)
            case = json_line(text)
            if case.get("status") != "pass" or case.get("validationErrors") != 0:
                raise RuntimeError(f"{scenario}: probe did not pass validation")
            report["cases"][scenario] = case
        wsi_env = dict(env, UW_LAYER_DISABLE="0", UW_PROBE_OMIT_MAINTENANCE_INSTANCE="1")
        text = run(["node", "tools/render-fixture/dist/wsi-smoke.js", str(output / "wsi")], output, "wsi", wsi_env, 110)
        report["cases"]["wsi"] = json_line(text)
        if report["cases"]["wsi"].get("status") != "pass":
            raise RuntimeError("WSI did not pass")
        # The no-host target still creates the layer but submits no overlay work.
        no_host_env = dict(wsi_env, UW_LAYER_ENABLE="1", UW_RENDER_PROBE="1",
                           UW_RENDER_STATS_DIR=str(output / "no-host"), UW_RENDER_TRACE_DIR=str(output / "trace"))
        text = run([str(ROOT / "target/debug/examples/uw-render-probe"), "--frames", "30"],
                   output, "no-host", no_host_env, 20)
        line = next(line[7:] for line in text.splitlines() if line.startswith("REPORT "))
        report["cases"]["no_host"] = json.loads(line)
        metrics = list((output / "no-host").glob("*-resources.json"))
        if not metrics or any(int(json.loads(p.read_text())["gpuBytes"]) != 0 for p in metrics):
            raise RuntimeError("no-host layer did not prove passthrough availability/zero uploads")
        traces = list((output / "trace").glob("*.json"))
        if not traces:
            raise RuntimeError("no-host present trace missing")
        for meta in traces:
            counts = json.loads(meta.read_text())
            rows = list(csv.DictReader(meta.with_suffix('.csv').read_text().splitlines()))
            if counts['status'] != 'complete' or counts['dropped'] or counts['submitted'] or counts['samples'] != len(rows) or len(rows) < 30:
                raise RuntimeError("invalid no-host trace")
        text = run(['node', 'tools/render-fixture/dist/widget-load-smoke.js', str(output / 'widget-load')], output, 'widget-load', wsi_env, 25)
        report['cases']['widget_load'] = json_line(text)
        if report['cases']['widget_load'].get('status') != 'pass':
            raise RuntimeError('supported widget load did not pass')
        report['cases']['trace'] = {'status':'pass','scope':'real loaded inactive layer, bounded trace, zero overlay submissions'}
        text = run(['node', 'tools/render-fixture/dist/host-smoke.js', str(output / 'host')], output, 'host', wsi_env, 40)
        report['cases']['host'] = json_line(text)
        if report['cases']['host'].get('status') != 'pass':
            raise RuntimeError('production host smoke failed')
        report["status"] = "pass"
    except (RuntimeError, ValueError, OSError, StopIteration) as error:
        report["status"] = "unavailable" if "unavailable:" in str(error) else "fail"
        report["error"] = str(error)
    finally:
        roots = [ROOT / "crates" / name for name in ("uw-frame-proto", "uw-frame-channel", "uw-render-bridge", "uw-vk-layer")]
        roots += [ROOT / "tools/render-fixture/src", ROOT / "tools/render-fixture/scripts", ROOT / "tools/render-fixture/widgets"]
        files = [p for folder in roots for p in folder.rglob("*") if p.is_file()]
        files += [ROOT / "Cargo.lock", Path(__file__).resolve(), ROOT / "tools/render-fixture/package-lock.json"]
        report["source_hashes"] = {str(p.relative_to(ROOT)): digest(p) for p in sorted(set(files))}
        artifacts = [ROOT / p for p in ("target/release/libuw_vk_layer.so", "target/debug/examples/gpu_probe", "target/debug/examples/uw-render-probe", "tools/render-fixture/native/render_bridge.node", "target/render-4/validation/usr/lib/libVkLayer_khronos_validation.so", "target/render-4/validation/validation.pkg.tar.zst")]
        report["artifact_hashes"] = {str(p.relative_to(ROOT)): digest(p) for p in artifacts if p.exists()}
        report["git_head"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        result_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{report['status']}: {result_path}")
    return 0 if report["status"] == "pass" else 1


PRODUCTION = ['uw-frame-proto', 'uw-frame-channel', 'uw-render-bridge', 'uw-vk-layer']
WINDOWS = ['uw-bootstrap', 'uw-game-channel', 'uw-helper', 'uw-host', 'uw-inject', 'uw-input', 'uw-overlay', 'uw-pipe-proto', 'uw-test-game', 'uw-test-overlay'] + PRODUCTION
ARTIFACTS = ['target/release/libuw_vk_layer.so', 'tools/render-fixture/native/render_bridge.node', 'target/release/uw-inject', 'target/x86_64-pc-windows-gnu/release/uw-helper64.exe', 'target/x86_64-pc-windows-gnu/release/uw_bootstrap64.dll', 'target/x86_64-pc-windows-gnu/release/uw_overlay.dll']


def artifact_inventory():
    return {p: {'sha256': digest(ROOT / p), 'size': (ROOT / p).stat().st_size} if (ROOT / p).is_file() else {'status':'missing'} for p in ARTIFACTS}


def process_snapshot(pid):
    proc = Path('/proc') / str(pid)
    ticks = proc.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]
    argv = proc.joinpath('cmdline').read_bytes().split(b'\0')
    return {'pid':pid, 'start_ticks':ticks, 'exe':os.readlink(proc/'exe'), 'argv':[p.decode(errors='replace') for p in argv if p]}


def inventory(pid, output):
    output.mkdir(parents=True, exist_ok=True)
    before = process_snapshot(pid)
    maps = Path(f'/proc/{pid}/maps').read_text()
    processes = []
    for proc in Path('/proc').iterdir():
        try:
            if proc.name.isdigit() and proc.stat().st_uid == os.getuid():
                item = process_snapshot(int(proc.name))
                if any(word in ' '.join(item['argv']).lower() for word in ('uw-inject','uw-helper','render-fixture','osr-host')):
                    processes.append(item)
        except (OSError, IndexError, ValueError):
            pass
    after = process_snapshot(pid)
    if before['start_ticks'] != after['start_ticks']:
        raise RuntimeError('process incarnation changed during inventory')
    (output/'maps.txt').write_text(maps)
    for name, command in [('vulkaninfo',['vulkaninfo','--summary']),('desktop',['xrandr','--current'])]:
        try: run(command,output,name,dict(os.environ, UW_LAYER_DISABLE='1', UW_SPIKE_LAYER_DISABLE='1'),20)
        except (RuntimeError,OSError) as error: (output/f'{name}-unavailable.txt').write_text(str(error))
    report = {'schema_version':1,'status':'pass','scope':'read-only process/module snapshot; focus/settings and full sample duration require operator evidence',
              'process':before, 'artifacts':artifact_inventory(), 'related_processes':processes,
              'loaded':{'production_layer':'libuw_vk_layer' in maps,'production_input':'uw_overlay.dll' in maps.lower(), 'bootstrap':'uw_bootstrap64.dll' in maps.lower(), 'spike':'uw_spike' in maps.lower() or 'libuw_spike_layer' in maps.lower()},
              'environment':{'platform':platform.platform(), **{k:os.environ.get(k) for k in ['DISPLAY','WAYLAND_DISPLAY','XDG_SESSION_TYPE','XDG_CURRENT_DESKTOP']}},
              'source_head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(), 'observations':{'focus':'pending','settings':'pending','uncapped':'pending'}}
    (output/'inventory.json').write_text(json.dumps(report,indent=2)+'\n')
    return 0


def automated(output):
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, UW_LAYER_DISABLE='1', UW_SPIKE_LAYER_DISABLE='1')
    packages = [v for name in PRODUCTION for v in ('-p',name)]
    win = [v for name in WINDOWS for v in ('-p',name)]
    strict = ['--','-D','warnings','-D','clippy::undocumented_unsafe_blocks']
    commands = [
        ('build-native',['npm','run','build:native','--prefix','tools/render-fixture']),
        ('workspace',['cargo','test','--workspace']),
        ('production',['cargo','test',*packages,'--all-targets']),
        ('clippy',['cargo','clippy',*packages,'--all-targets',*strict]),
        ('clippy-node',['cargo','clippy','-p','uw-render-bridge','--all-targets','--features','node',*strict]),
        ('clippy-windows',['cargo','clippy',*packages,'--all-targets','--target','x86_64-pc-windows-gnu',*strict]),
        ('windows',['cargo','check',*win,'--all-targets','--target','x86_64-pc-windows-gnu']),
        ('node',['npm','test','--prefix','tools/render-fixture']),
        ('addon',['npm','run','test:addon','--prefix','tools/render-fixture']),
        ('recovery',['python3','scripts/render-4-recovery-smoke.py']),
        ('qualification',['xvfb-run','-a','npm','run','qualify','--prefix','tools/render-fixture','--','--output',str(output/'images')]),
        ('renderer',['xvfb-run','-a','npm','run','test:renderer','--prefix','tools/render-fixture']),
        ('retained-input',['npm','run','test:production-input','--prefix','spikes/osr-host']),
        ('retained-routing',['npm','run','test:production-routing','--prefix','spikes/osr-host']),
        ('report',['python3','-m','unittest','discover','-s','scripts/tests','-p','test_render_4_report.py']),
        ('format',['cargo','fmt',*packages,'--check']),
        ('shell',['bash','-n','scripts/render-4-build.sh','scripts/render-4-install.sh','scripts/render-4-launch.sh']),
    ]
    report = {'schema_version':1,'status':'running','source_head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'checks':{},'scope':'automated owned tests only; no operator game, install or benchmark'}
    for name, command in commands:
        print(f'running {name}',flush=True)
        started = time.monotonic()
        try:
            run(command,output,name,env,600)
            value = {'status':'pass','exit_status':0}
        except (RuntimeError, OSError) as error:
            value = {'status':'fail','exit_status':getattr(error, 'code', 127),'error':str(error)}
        report['checks'][name] = {**value,'command':command,'seconds':time.monotonic()-started,'raw':str(output/f'{name}.log')}
        (output/'results.json').write_text(json.dumps(report,indent=2)+'\n')
    report['status'] = 'pass' if all(c['status']=='pass' for c in report['checks'].values()) else 'fail'
    report['artifacts'] = artifact_inventory()
    (output/'results.json').write_text(json.dumps(report,indent=2)+'\n')
    print(f"{report['status']}: {output/'results.json'}",flush=True)
    return int(report['status'] != 'pass')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--gpu',action='store_true')
    mode.add_argument('--automated',action='store_true')
    mode.add_argument('--inventory',action='store_true')
    parser.add_argument('--pid',type=int)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if args.inventory:
        if args.pid is None or args.pid <= 0: parser.error('--inventory needs a positive --pid')
        return inventory(args.pid,output)
    return automated(output) if args.automated else gpu_check(output)


if __name__ == "__main__":
    sys.exit(main())
