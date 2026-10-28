const { execFileSync } = require('node:child_process');
const { mkdirSync, copyFileSync, renameSync } = require('node:fs');
const { resolve } = require('node:path');
const root = resolve(__dirname, '../../..');
execFileSync('cargo', ['build', '--release', '-p', 'uw-render-bridge', '--features', 'node'], { cwd: root, stdio: 'inherit' });
execFileSync('cargo', ['build', '-p', 'uw-render-bridge', '--example', 'addon_consumer'], { cwd: root, stdio: 'inherit' });
const dest = resolve(__dirname, '../native'); mkdirSync(dest, { recursive: true });
copyFileSync(resolve(root, 'target/release/libuw_render_bridge.so'), resolve(dest, 'render_bridge.node.tmp'));
renameSync(resolve(dest, 'render_bridge.node.tmp'), resolve(dest, 'render_bridge.node'));
