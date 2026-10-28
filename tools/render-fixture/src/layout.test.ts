import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { layout } from './layout';
test('benchmark keeps the approved physical workloads and hides all targets if the swapchain cannot fit', () => {
  const large = layout('benchmark', { width: 1920, height: 1080 });
  assert.deepEqual(large.map(w => [w.width, w.height]), [[400, 300], [300, 200], [300, 200]]);
  assert(large.every(w => w.visible));
  assert(layout('benchmark', { width: 640, height: 480 }).every(w => !w.visible));
});
test('overlapping input bounds are the same physical rectangles used for drawing', () => {
  const targets = layout('input', { width: 1000, height: 800 });
  assert.equal(targets.length, 2); assert(targets.every(w => w.visible));
  assert(targets[1].x < targets[0].x + targets[0].width);
  assert(targets[1].y < targets[0].y + targets[0].height);
  assert.notEqual(targets[0].z, targets[1].z);
});
