#!/usr/bin/env bash
# Build only; never install, inject or start a game.
set -euo pipefail
uw_render_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$uw_render_root"
cargo build --release -p uw-vk-layer -p uw-inject
cargo build --release --target x86_64-pc-windows-gnu -p uw-helper -p uw-bootstrap -p uw-overlay
if [[ ! -d tools/render-fixture/node_modules ]]; then npm ci --prefix tools/render-fixture; fi
npm run build:native --prefix tools/render-fixture
npm run build --prefix tools/render-fixture
