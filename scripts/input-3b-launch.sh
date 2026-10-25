#!/usr/bin/env bash
# Test-only v2 production session. The Electron host supplies the widget table.
set -euo pipefail
uw_routing_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
uw_routing_win="$uw_routing_root/target/x86_64-pc-windows-gnu/release"
uw_routing_args=(--exe "${UW_INPUT_EXE:-deadlock.exe}" --helper "$uw_routing_win/uw-helper64.exe" --bootstrap "$uw_routing_win/uw_bootstrap64.dll" --overlay "$uw_routing_win/uw_overlay.dll")
if [[ -n ${UW_INPUT_PID:-} ]]; then uw_routing_args+=(--pid "$UW_INPUT_PID"); fi
if [[ -n ${UW_INPUT_PROTON:-} ]]; then uw_routing_args+=(--proton "$UW_INPUT_PROTON"); fi
exec "$uw_routing_root/target/release/uw-inject" "${uw_routing_args[@]}" "$@"
