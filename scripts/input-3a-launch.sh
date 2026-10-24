#!/usr/bin/env bash
# Test-only production input launcher. No spike input injector or collector is loaded.
set -euo pipefail
uw_input_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
uw_input_win="$uw_input_root/target/x86_64-pc-windows-gnu/release"
uw_input_args=(--exe "${UW_INPUT_EXE:-deadlock.exe}" --helper "$uw_input_win/uw-helper64.exe" --bootstrap "$uw_input_win/uw_bootstrap64.dll" --overlay "$uw_input_win/uw_overlay.dll")
if [[ -n ${UW_INPUT_PID:-} ]]; then uw_input_args+=(--pid "$UW_INPUT_PID"); fi
if [[ -n ${UW_INPUT_PROTON:-} ]]; then uw_input_args+=(--proton "$UW_INPUT_PROTON"); fi
exec "$uw_input_root/target/release/uw-inject" "${uw_input_args[@]}" "$@"
