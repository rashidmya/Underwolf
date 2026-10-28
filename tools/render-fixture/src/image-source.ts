import type { NativeImage } from 'electron';
export type RectLike = { x: number; y: number; width: number; height: number };
export type ImageMode = 'retained-full' | 'owned-backing';
export type PixelPatch = RectLike & { stride: number; data: Buffer };

export class ImageSource {
  private retained?: NativeImage;
  private backing?: Buffer;
  private closed = false;
  private readonly dimensions: { width: number; height: number };

  constructor(private readonly mode: ImageMode, size: { width: number; height: number }) {
    if (!['retained-full', 'owned-backing'].includes(mode)
      || ![size.width, size.height].every(n => Number.isInteger(n) && n > 0 && n <= 4096)) {
      throw Error('invalid image source dimensions or mode');
    }
    this.dimensions = { width: size.width, height: size.height };
  }

  private check(rect: RectLike, empty = false): void {
    if (this.closed) throw Error('image source disposed');
    if (![rect.x, rect.y, rect.width, rect.height].every(n => Number.isSafeInteger(n) && n >= 0)
      || (!empty && (!rect.width || !rect.height))
      || rect.x + rect.width > this.dimensions.width
      || rect.y + rect.height > this.dimensions.height) throw Error('invalid image rectangle');
  }

  private pixels(image: NativeImage, width: number, height: number): Buffer {
    const size = image.getSize();
    if (size.width !== width || size.height !== height) throw Error('unexpected bitmap dimensions');
    const data = image.toBitmap();
    if (data.length !== width * height * 4) throw Error('unexpected BGRA bitmap length');
    return data;
  }

  accept(image: NativeImage, dirty: RectLike): void {
    this.check(dirty, true);
    if (!dirty.width || !dirty.height) return;
    const size = image.getSize();
    const full = size.width === this.dimensions.width && size.height === this.dimensions.height;
    if (this.mode === 'retained-full') {
      if (!full) throw Error('retained image must contain the complete frame');
      this.retained = image;
      return;
    }
    if (!this.backing) {
      if (!full) throw Error('initial image must contain the complete frame');
      this.backing = this.pixels(image, size.width, size.height);
      return;
    }
    const patch = full ? image.crop(dirty) : image;
    // Validate the complete source before any write to the retained baseline.
    const bytes = this.pixels(patch, dirty.width, dirty.height);
    const rowBytes = dirty.width * 4;
    for (let y = 0; y < dirty.height; y++) {
      bytes.copy(this.backing, ((dirty.y + y) * this.dimensions.width + dirty.x) * 4,
        y * rowBytes, (y + 1) * rowBytes);
    }
  }

  extract(rect: RectLike): PixelPatch {
    this.check(rect);
    const stride = rect.width * 4;
    if (this.mode === 'retained-full') {
      if (!this.retained) throw Error('image baseline unavailable');
      return { ...rect, stride, data: this.pixels(this.retained.crop(rect), rect.width, rect.height) };
    }
    if (!this.backing) throw Error('image baseline unavailable');
    const data = Buffer.allocUnsafe(stride * rect.height);
    for (let y = 0; y < rect.height; y++) {
      const start = ((rect.y + y) * this.dimensions.width + rect.x) * 4;
      this.backing.copy(data, y * stride, start, start + stride);
    }
    return { ...rect, stride, data };
  }

  size(): { width: number; height: number } { return { ...this.dimensions }; }
  dispose(): void { this.closed = true; this.retained = undefined; this.backing = undefined; }
}
