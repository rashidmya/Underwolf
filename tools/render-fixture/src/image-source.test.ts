import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import type { NativeImage } from 'electron';
import { ImageSource, type ImageMode, type RectLike } from './image-source';

const FULL = { x: 0, y: 0, width: 64, height: 48 };
const A = { x: 8, y: 8, width: 4, height: 4 };
const B = { x: 40, y: 30, width: 4, height: 4 };
const background = [64, 32, 16, 255];
const red = [0, 0, 255, 255];
const green = [0, 255, 0, 255];

function fill(bytes: Buffer, rect: RectLike, color: number[], width = 64): void {
  for (let y = rect.y; y < rect.y + rect.height; y++) {
    for (let x = rect.x; x < rect.x + rect.width; x++) {
      bytes.set(color, (y * width + x) * 4);
    }
  }
}
function frame(): Buffer {
  const bytes = Buffer.alloc(64 * 48 * 4);
  fill(bytes, FULL, background);
  return bytes;
}
// A byte-backed image adapter for the source's own pixel-copy contract. Actual
// NativeImage shape/lifetime/colour assumptions are qualified in Electron.
function image(bytes: Buffer, width = 64, height = 48): NativeImage {
  return {
    getSize: () => ({ width, height }),
    isEmpty: () => width === 0 || height === 0,
    toBitmap: () => Buffer.from(bytes),
    crop: (rect: RectLike) => {
      const patch = Buffer.alloc(rect.width * rect.height * 4);
      for (let row = 0; row < rect.height; row++) {
        bytes.copy(patch, row * rect.width * 4,
          ((rect.y + row) * width + rect.x) * 4,
          ((rect.y + row) * width + rect.x + rect.width) * 4);
      }
      return image(patch, rect.width, rect.height);
    },
  } as unknown as NativeImage;
}

for (const mode of ['retained-full', 'owned-backing'] as const) {
  test(`${mode}: disjoint paints and transparent clearing match a complete reference`, () => {
    const source = new ImageSource(mode, { width: 64, height: 48 });
    const expected = frame();
    source.accept(image(Buffer.from(expected)), FULL);
    fill(expected, A, red);
    source.accept(image(Buffer.from(expected)), A);
    fill(expected, B, green);
    source.accept(image(Buffer.from(expected)), B);
    fill(expected, A, [0, 0, 0, 0]);
    source.accept(image(Buffer.from(expected)), A);
    assert.deepEqual(source.extract(FULL).data, expected);
    assert.deepEqual(source.extract(B), { ...B, stride: 16,
      data: Buffer.from(Array(16).fill(green).flat()) });
  });

  test(`${mode}: sparse first notification still creates a complete baseline`, () => {
    const source = new ImageSource(mode, { width: 64, height: 48 });
    const bytes = frame();
    source.accept(image(bytes), A);
    assert.deepEqual(source.extract(FULL).data, bytes);
  });

  test(`${mode}: invalid rectangles cannot read beyond the image`, () => {
    const source = new ImageSource(mode, { width: 64, height: 48 });
    source.accept(image(frame()), FULL);
    for (const rect of [
      { x: -1, y: 0, width: 1, height: 1 },
      { x: 63, y: 0, width: 2, height: 1 },
      { x: 0, y: 48, width: 1, height: 1 },
      { x: 0, y: 0, width: 0, height: 1 },
      { x: NaN, y: 0, width: 1, height: 1 },
      { x: 0, y: 0, width: 1.5, height: 1 },
    ]) assert.throws(() => source.extract(rect));
  });

  test(`${mode}: disposal cannot expose a previous image`, () => {
    const source = new ImageSource(mode, { width: 64, height: 48 });
    source.accept(image(frame()), FULL);
    source.dispose();
    assert.throws(() => source.extract(FULL));
    assert.throws(() => source.accept(image(frame()), FULL));
  });
}

test('owned-backing: a partial bitmap updates only its correctly positioned rows', () => {
  const source = new ImageSource('owned-backing', { width: 64, height: 48 });
  const expected = frame();
  source.accept(image(Buffer.from(expected)), FULL);
  const patch = Buffer.from(Array(16).fill(red).flat());
  source.accept(image(patch, 4, 4), A);
  patch.fill(0); // producer backing must own the accepted bytes
  fill(expected, A, red);
  assert.deepEqual(source.extract(FULL).data, expected);
});

test('partial first image and wrong full dimensions cannot establish a baseline', () => {
  for (const mode of ['retained-full', 'owned-backing'] as ImageMode[]) {
    const source = new ImageSource(mode, { width: 64, height: 48 });
    assert.throws(() => source.accept(image(Buffer.alloc(64), 4, 4), A));
    assert.throws(() => source.accept(image(Buffer.alloc(60 * 48 * 4), 60, 48), FULL));
    assert.throws(() => source.extract(FULL));
  }
});

test('malformed bitmap lengths are rejected before changing retained pixels', () => {
  const source = new ImageSource('owned-backing', { width: 64, height: 48 });
  const expected = frame();
  source.accept(image(Buffer.from(expected)), FULL);
  assert.throws(() => source.accept(image(Buffer.alloc(63), 4, 4), A));
  assert.deepEqual(source.extract(FULL).data, expected);
});

test('invalid configured dimensions cannot allocate an oversized image', () => {
  for (const size of [{ width: 0, height: 48 }, { width: 4097, height: 1 },
    { width: Infinity, height: 1 }, { width: 64, height: 1.5 }]) {
    assert.throws(() => new ImageSource('owned-backing', size));
  }
});
