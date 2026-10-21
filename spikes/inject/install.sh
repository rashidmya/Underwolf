#!/usr/bin/env bash
# Builds the C3 hook DLL, the Windows injector and the Linux launcher, and installs them next to
# the spike layer. The host (--capture win32) runs the launcher when a game's layer connects.
# Also builds Spike 3's collector plugin DLL, the Wine-side GEP helper and the `uw-gep` client.
# Also reinstalls the layer: a layer older than the C3 wiring ignores UW_SPIKE_CAPTURE=win32 and
# runs C2 alongside the hook (seen in the first in-game C3 run).
# Needs the x86_64-pc-windows-gnu Rust target and mingw (`sudo pacman -S mingw-w64-gcc`).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
manifest="$root/Cargo.toml"

"$root/spikes/vk-layer/install.sh"

cargo build --release --manifest-path "$manifest" --target x86_64-pc-windows-gnu \
    -p uw-spike-input -p uw-spike-inject -p uw-spike-dlcollector -p uw-spike-gep-helper
cargo build --release --manifest-path "$manifest" -p uw-spike-inject --bin uw-spike-inject -p uw-spike-gep-host --bin uw-gep

dir="$HOME/.local/share/underwolf-spike"
mkdir -p "$dir"
win="$root/target/x86_64-pc-windows-gnu/release"
# Copy then rename, so a game that already loaded the DLL keeps its copy intact.
for src in "$win/uw_spike_input.dll" "$win/uw-spike-inject-win.exe" "$win/uw_spike_dlcollector.dll" \
    "$win/uw-gep-helper.exe" "$root/target/release/uw-spike-inject" "$root/target/release/uw-gep"; do
    dst="$dir/$(basename "$src")"
    cp "$src" "$dst.tmp"
    mv -f "$dst.tmp" "$dst"
    echo "installed: $dst"
done
