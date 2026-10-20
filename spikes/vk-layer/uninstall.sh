#!/usr/bin/env bash
set -euo pipefail
rm -f "$HOME/.local/share/vulkan/implicit_layer.d/underwolf_spike_layer.json"
rm -f "$HOME/.local/share/underwolf-spike/libuw_spike_layer.so"
echo "uninstalled VK_LAYER_UNDERWOLF_spike"
