#!/usr/bin/env python3
"""Owned GPU/WSI validation probes; never installs a layer or touches a game."""
from __future__ import annotations
import argparse
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
        raise RuntimeError(f"{name}: owned process group exceeded {timeout}s")
    (output / f"{name}.log").write_text(text)
    if process.returncode:
        raise RuntimeError(f"{name} exited {process.returncode}: {text[-3000:]}")
    return text


def json_line(text: str) -> dict:
    for line in reversed(text.splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    raise ValueError("probe returned no JSON report")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpu", action="store_true", required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "target/render-4/gpu")
    args = parser.parse_args()
    output = args.output.resolve()
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
                           UW_RENDER_STATS_DIR=str(output / "no-host"))
        text = run([str(ROOT / "target/debug/examples/uw-render-probe"), "--frames", "30"],
                   output, "no-host", no_host_env, 20)
        line = next(line[7:] for line in text.splitlines() if line.startswith("REPORT "))
        report["cases"]["no_host"] = json.loads(line)
        metrics = list((output / "no-host").glob("*-resources.json"))
        if not metrics or any(int(json.loads(p.read_text())["gpuBytes"]) != 0 for p in metrics):
            raise RuntimeError("no-host layer did not prove passthrough availability/zero uploads")
        report["status"] = "pass"
    except (RuntimeError, ValueError, OSError, StopIteration) as error:
        report["status"] = "unavailable" if "unavailable:" in str(error) else "fail"
        report["error"] = str(error)
    finally:
        roots = [ROOT / "crates" / name for name in ("uw-frame-proto", "uw-frame-channel", "uw-render-bridge", "uw-vk-layer")]
        roots += [ROOT / "tools/render-fixture/src", ROOT / "tools/render-fixture/scripts"]
        files = [p for folder in roots for p in folder.rglob("*") if p.is_file()]
        files += [ROOT / "Cargo.lock", Path(__file__).resolve(), ROOT / "tools/render-fixture/package-lock.json"]
        report["source_hashes"] = {str(p.relative_to(ROOT)): digest(p) for p in sorted(set(files))}
        artifacts = [ROOT / p for p in ("target/release/libuw_vk_layer.so", "target/debug/examples/gpu_probe", "target/debug/examples/uw-render-probe", "tools/render-fixture/native/render_bridge.node", "target/render-4/validation/usr/lib/libVkLayer_khronos_validation.so", "target/render-4/validation/validation.pkg.tar.zst")]
        report["artifact_hashes"] = {str(p.relative_to(ROOT)): digest(p) for p in artifacts if p.exists()}
        report["git_head"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        result_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{report['status']}: {result_path}")
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
