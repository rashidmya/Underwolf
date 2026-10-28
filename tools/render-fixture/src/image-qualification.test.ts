import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import type { NativeImage } from 'electron';
import { ImageQualification } from './image-qualification';
const image = (bytes: Buffer): NativeImage => ({ getSize: () => ({ width: 2, height: 2 }), toBitmap: () => Buffer.from(bytes) } as NativeImage);
test('a full-sized later image with invalid pixels outside damage cannot qualify for retention', () => {
  const q = new ImageQualification({ width: 2, height: 2 });
  q.capture(image(Buffer.alloc(16,7)), Buffer.alloc(16,7));
  const expected = Buffer.alloc(16,7); expected.fill(9,0,4);
  const dirtyOnly = Buffer.alloc(16); dirtyOnly.fill(9,0,4);
  q.capture(image(dirtyOnly), expected);
  assert.equal(q.result().completeImages,false);
});
test('an intermediate snapshot must remain stable through subsequent paints', () => {
  const q = new ImageQualification({ width: 2, height: 2 });
  q.capture(image(Buffer.alloc(16,7)),Buffer.alloc(16,7));
  const middle = Buffer.alloc(16,9); q.capture(image(middle),Buffer.alloc(16,9));
  middle.fill(11); q.capture(image(Buffer.alloc(16,13)),Buffer.alloc(16,13));
  assert.equal(q.result().snapshotStable,false);
});
