// A bounded characterization of the pinned Electron API, not an app runtime.
import { app, BrowserWindow, type NativeImage } from 'electron';
import { strict as assert } from 'node:assert';
import { mkdirSync } from 'node:fs';
import { mkdir, writeFile } from 'node:fs/promises';
import * as path from 'node:path';
import { ImageSource, type RectLike } from './image-source';

const outputAt = process.argv.indexOf('--output');
const output = outputAt < 0 ? path.resolve(__dirname, '../../../target/render-4/image-contract') : process.argv[outputAt + 1];
if (!output || !path.isAbsolute(output)) throw Error('--output requires an absolute directory');
mkdirSync(path.join(output, 'profile'), { recursive: true });
app.setPath('userData', path.join(output, 'profile'));
if (process.argv.includes('--software')) app.disableHardwareAcceleration();
// The two isolated windows close between scenarios; only this runner decides
// when reports are durable and the process may exit.
app.on('window-all-closed', () => {});

function fill(bytes: Buffer, width: number, rect: RectLike, color: number[]): void {
  for (let y = rect.y; y < rect.y + rect.height; y++) {
    for (let x = rect.x; x < rect.x + rect.width; x++) bytes.set(color, (y * width + x) * 4);
  }
}

async function qualify(scale: number) {
  const width = 64 * scale, height = 48 * scale;
  const full = { x: 0, y: 0, width, height };
  const source = new ImageSource('owned-backing', { width, height });
  const win = new BrowserWindow({ show: false, width: 64, height: 48, transparent: true,
    backgroundColor: '#00000000', webPreferences: { offscreen: { useSharedTexture: false, deviceScaleFactor: scale } } });
  win.webContents.setFrameRate(60);
  let last: { image: NativeImage; dirty: RectLike } | undefined;
  let error: unknown;
  const shapes: unknown[] = [];
  const sizes: { width: number; height: number }[] = [];
  win.webContents.on('paint', (_event, dirty, image) => {
    try {
      const size = image.getSize();
      if (shapes.length < 32) shapes.push({ dirty, size, bytes: image.toBitmap().length });
      sizes.push(size);
      source.accept(image, dirty);
      last = { image, dirty };
    } catch (caught) { error = caught; }
  });
  const waitFor = async (rect: RectLike, color: number[]) => {
    const deadline = performance.now() + 5000;
    while (performance.now() < deadline) {
      if (error) throw error;
      if (last) {
        const bytes = source.extract({ x: rect.x, y: rect.y, width: 1, height: 1 }).data;
        if (bytes.equals(Buffer.from(color))) return;
      }
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    const sample = last ? [...source.extract({ x: rect.x, y: rect.y, width: 2, height: 2 }).data] : [];
    const canvas = await win.webContents.executeJavaScript(`({dpr:devicePixelRatio,width:document.querySelector('canvas').width,height:document.querySelector('canvas').height})`);
    throw Error(`paint timeout at scale ${scale}: ${JSON.stringify({ expected: color, sample, canvas, shapes })}`);
  };
  try {
    await win.loadFile(path.join(__dirname, '../widgets/image-contract.html'));
    await waitFor(full, [64, 32, 16, 255]);
    assert.ok(last);
    const initial = last.image;
    const initialBytes = initial.toBitmap();
    const reference = Buffer.alloc(width * height * 4);
    fill(reference, width, full, [64, 32, 16, 255]);
    assert.deepEqual(source.extract(full).data, reference);
    const a = { x: 8 * scale, y: 8 * scale, width: 4 * scale, height: 4 * scale };
    const b = { x: 40 * scale, y: 30 * scale, width: 4 * scale, height: 4 * scale };
    let snapshotStable = true;
    let extractionNs = 0n, extractedBytes = 0;
    for (const [step, rect, color] of [[1, a, [0, 0, 255, 255]], [2, b, [0, 255, 0, 255]],
      [3, a, [0, 0, 0, 0]]] as const) {
      await win.webContents.executeJavaScript(`window.drawStep(${step})`);
      await waitFor(rect, [...color]);
      fill(reference, width, rect, [...color]);
      const began = process.hrtime.bigint();
      const actual = source.extract(full).data;
      extractionNs += process.hrtime.bigint() - began;
      extractedBytes += actual.length;
      assert.deepEqual(actual, reference, `incorrect complete pixels at scale ${scale}, step ${step}`);
      (globalThis as { gc?: () => void }).gc?.();
      snapshotStable &&= initial.toBitmap().equals(initialBytes);
    }
    assert.ok(last);
    const complete = sizes.every(size => size.width === width && size.height === height);
    let cropped = false;
    if (complete) {
      const patch = last.image.crop(b);
      const expected = Buffer.from(Array(b.width * b.height).fill([0, 255, 0, 255]).flat());
      cropped = patch.getSize().width === b.width && patch.getSize().height === b.height && patch.toBitmap().equals(expected);
      assert.equal(cropped, true, 'NativeImage crop must preserve physical-pixel regions');
    }
    return { scale, dimensions: { width, height }, completeImages: complete, snapshotStable,
      cropped, selectedMode: complete && snapshotStable && cropped ? 'retained-full' : 'owned-backing',
      extractionNs: String(extractionNs), extractedBytes, gcAvailable: typeof (globalThis as { gc?: unknown }).gc === 'function', shapes };
  } finally { source.dispose(); win.destroy(); }
}

const timeout = setTimeout(() => { console.error('image qualification exceeded 30 seconds'); app.exit(1); }, 30_000);
app.whenReady().then(async () => {
  await mkdir(output, { recursive: true });
  const results = [];
  try {
    for (const scale of [1, 1.5]) results.push(await qualify(scale));
    let gpuInfo: unknown;
    try { gpuInfo = await app.getGPUInfo('basic'); }
    catch (error) { gpuInfo = { unavailable: String(error) }; }
    const report = { electron: process.versions.electron, chromium: process.versions.chrome,
      requestedSoftware: process.argv.includes('--software'),
      gpu: app.getGPUFeatureStatus(), gpuInfo, results,
      scope: 'isolated paint qualification; no game, input module or render layer started' };
    await writeFile(path.join(output, 'qualification.json'), JSON.stringify(report, null, 2) + '\n');
    console.log(`PASS Electron ${report.electron}: complete BGRA pixels, disjoint changes, transparent clearing and retained snapshots at scales 1 and 1.5`);
    console.log(results.map(result => `scale ${result.scale}: ${result.selectedMode}`).join('\n'));
    clearTimeout(timeout); app.exit(0);
  } catch (error) {
    console.error(error);
    await writeFile(path.join(output, 'failure.json'), JSON.stringify({ error: String(error), results }, null, 2) + '\n');
    clearTimeout(timeout); app.exit(1);
  }
}).catch(error => { console.error(error); clearTimeout(timeout); app.exit(1); });
