#!/usr/bin/env bash
# Builds the spike layer and installs it as a user-level Vulkan implicit layer.
# Disable per process with UW_SPIKE_LAYER_DISABLE=1; remove with uninstall.sh.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

cargo build --release -p uw-spike-layer --manifest-path "$root/Cargo.toml"

lib_dir="$HOME/.local/share/underwolf-spike"
manifest_dir="$HOME/.local/share/vulkan/implicit_layer.d"
lib="$lib_dir/libuw_spike_layer.so"
mkdir -p "$lib_dir" "$manifest_dir"

# Copy then rename, so running processes keep their mapped copy intact.
cp "$root/target/release/libuw_spike_layer.so" "$lib.tmp"
mv -f "$lib.tmp" "$lib"

sed "s|@LIBRARY_PATH@|$lib|" "$here/underwolf_spike_layer.json.in" > "$manifest_dir/underwolf_spike_layer.json"

echo "installed: $lib"
echo "manifest:  $manifest_dir/underwolf_spike_layer.json"
