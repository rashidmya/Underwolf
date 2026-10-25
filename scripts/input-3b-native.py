#!/usr/bin/env python3
"""Run one native input fixture in a fresh prefix and preserve parsed evidence."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", choices=("wine", "proton"), required=True)
    parser.add_argument("--proton", type=Path, default=os.environ.get("UW_INPUT_PROTON"))
    parser.add_argument("--case", default="directinput", help="input_probe routing case")
    parser.add_argument("--discover", action="store_true", help="ABI availability; never isolation acceptance")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if args.runtime == "proton" and not args.proton:
        parser.error("--proton or UW_INPUT_PROTON must select the existing runtime")
    root = Path(__file__).resolve().parents[1]
    tag = "discovery" if args.discover else args.case
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    folder = (args.out or root / "target/input-3b-native" / f"{stamp}-{args.runtime}-{tag}").resolve()
    folder.mkdir(parents=True, exist_ok=False)
    example = "native_input_probe.exe" if args.discover else "input_probe.exe"
    binary = root / "target/x86_64-pc-windows-gnu/release/examples" / example
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    arguments = ["--discover", "--directinput", "--preexisting", "--late-load"] if args.discover else ["--routing", args.case]
    environment = os.environ.copy()
    environment.pop("WINEPREFIX", None)
    environment.update(WINEDEBUG="-all", WINEDLLOVERRIDES="mscoree,mshtml=")
    with tempfile.TemporaryDirectory(prefix="uw-input-3b-native-") as prefix:
        if args.runtime == "wine":
            environment["WINEPREFIX"] = prefix
            command = ["wine", str(binary), *arguments]
            server = "wineserver"
        else:
            proton = args.proton.resolve()
            environment.update(STEAM_COMPAT_DATA_PATH=prefix,
                               STEAM_COMPAT_CLIENT_INSTALL_PATH=str(proton.parents[2]),
                               SteamAppId="0", SteamGameId="0", PROTON_LOG="1", PROTON_LOG_DIR=str(folder))
            command = [str(proton / "proton"), "runinprefix", str(binary), *arguments]
            server = str(proton / "files/bin/wineserver")
        try:
            with (folder / "stdout.log").open("w") as stdout, (folder / "stderr.log").open("w") as stderr:
                process = subprocess.run(["xvfb-run", "-a", *command], env=environment, stdout=stdout, stderr=stderr, timeout=90)
            output = (folder / ("stdout.log" if args.runtime == "wine" else "steam-0.log")).read_text(errors="replace")
            record = dict(runtime=args.runtime, case=tag, exit=process.returncode, sha256=digest,
                          scope="isolated ABI discovery" if args.discover else "isolated native routing fixture",
                          owner_acceptance="open", gameinput_acceptance="open")
            if args.discover:
                probes = []
                for line in output.splitlines():
                    if line.lstrip().startswith("{"):
                        try:
                            value = json.loads(line)
                            if "directinput_abis" in value and "gameinput_detail" in value:
                                probes.append(value)
                        except json.JSONDecodeError:
                            pass
                record["probe"] = probes[0] if len(probes) == 1 else None
                record["passed"] = process.returncode == 0 and len(probes) == 1
            else:
                record["passed"] = process.returncode == 0 and any(
                    line.strip() == f"PASS routing probe {args.case}" for line in output.splitlines())
            (folder / "result.json").write_text(json.dumps(record, indent=2) + "\n")
            print(json.dumps({**record, "evidence": str(folder)}), flush=True)
            if not record["passed"]:
                raise SystemExit(1)
        finally:
            environment["WINEPREFIX"] = prefix if args.runtime == "wine" else str(Path(prefix) / "pfx")
            subprocess.run([server, "-k"], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)


if __name__ == "__main__":
    main()
