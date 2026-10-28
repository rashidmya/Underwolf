#!/usr/bin/env bash
# Production input launcher used by the explicitly targeted rendering fixture.
set -euo pipefail
uw_render_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
uw_render_win="$uw_render_root/target/x86_64-pc-windows-gnu/release"
uw_render_inject=${UW_RENDER_INJECT:-$uw_render_root/target/release/uw-inject}
uw_render_helper=${UW_RENDER_HELPER:-$uw_render_win/uw-helper64.exe}
uw_render_bootstrap=${UW_RENDER_BOOTSTRAP:-$uw_render_win/uw_bootstrap64.dll}
uw_render_overlay=${UW_RENDER_OVERLAY:-$uw_render_win/uw_overlay.dll}
: "${UW_INPUT_PID:?set the explicit target PID}"
: "${UW_INPUT_START_TICKS:?set the target process incarnation}"
for uw_render_file in "$uw_render_inject" "$uw_render_helper" "$uw_render_bootstrap" "$uw_render_overlay"; do
  [[ $uw_render_file = /* && -f $uw_render_file ]] || { echo "missing absolute artifact: $uw_render_file" >&2; exit 1; }
done
# Re-check immediately before launching; the bridge separately authenticates this incarnation.
python3 - "$UW_INPUT_PID" "$UW_INPUT_START_TICKS" <<'PY'
import pathlib, sys
stat = pathlib.Path('/proc', sys.argv[1], 'stat').read_text().rsplit(')', 1)[1].split()
if stat[19] != sys.argv[2]:
    raise SystemExit('target process incarnation changed')
PY
uw_render_args=(--pid "$UW_INPUT_PID" --exe "${UW_INPUT_EXE:-deadlock.exe}" --helper "$uw_render_helper" --bootstrap "$uw_render_bootstrap" --overlay "$uw_render_overlay")
if [[ -n ${UW_INPUT_PROTON:-} ]]; then uw_render_args+=(--proton "$UW_INPUT_PROTON"); fi
exec "$uw_render_inject" "${uw_render_args[@]}" "$@"
