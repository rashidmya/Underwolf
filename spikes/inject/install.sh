#!/usr/bin/env bash
# Builds the C3 hook DLL, the Windows injector and the Linux launcher, and installs them next to
# the spike layer. The host (--capture win32) runs the launcher when a game's layer connects.
# Needs the x86_64-pc-windows-gnu Rust target and mingw (`sudo pacman -S mingw-w64-gcc`).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
manifest="$root/Cargo.toml"

cargo build --release --manifest-path "$manifest" --target x86_64-pc-windows-gnu -p uw-spike-input -p uw-spike-inject
cargo build --release --manifest-path "$manifest" -p uw-spike-inject --bin uw-spike-inject

dir="$HOME/.local/share/underwolf-spike"
mkdir -p "$dir"
win="$root/target/x86_64-pc-windows-gnu/release"
# Copy then rename, so a game that already loaded the DLL keeps its copy intact.
for src in "$win/uw_spike_input.dll" "$win/uw-spike-inject-win.exe" "$root/target/release/uw-spike-inject"; do
    dst="$dir/$(basename "$src")"
    cp "$src" "$dst.tmp"
    mv -f "$dst.tmp" "$dst"
    echo "installed: $dst"
done
