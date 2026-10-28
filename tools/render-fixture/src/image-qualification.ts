import type { NativeImage } from 'electron';
export class ImageQualification {
  private frames: Array<{ image: NativeImage; bytes: Buffer }> = [];
  private complete = true;
  constructor(private readonly size: { width: number; height: number }) {}
  capture(image: NativeImage, reference: Buffer): void {
    const actual = image.getSize(), bytes = image.toBitmap();
    this.complete &&= actual.width === this.size.width && actual.height === this.size.height && bytes.equals(reference);
    this.frames.push({ image, bytes: Buffer.from(bytes) });
  }
  result(): { completeImages: boolean; snapshotStable: boolean; checkedSnapshots: number } {
    return { completeImages: this.frames.length > 0 && this.complete, snapshotStable: this.frames.length > 0 && this.frames.every(frame => frame.image.toBitmap().equals(frame.bytes)), checkedSnapshots: this.frames.length };
  }
}
