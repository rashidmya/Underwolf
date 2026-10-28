import { BrowserWindow } from 'electron';
import type { Bridge } from './bridge-types';
import { resolve } from 'node:path';
import { ImageSource, type ImageMode } from './image-source';
import { applySurface, localPoint, type Application, type SurfaceEvent } from './input-consumer';
import type { Target } from './layout';
import { WidgetProducer } from './producer';
export type Surface = { win: BrowserWindow; producer: WidgetProducer; apply(event: SurfaceEvent, application: Application): Promise<void>; invalidate(generation: number): void; close(): void };
export async function createSurface(target: Target, bridge: Bridge, options: { scale: number; copyMode: 'dirty' | 'full'; imageMode: ImageMode; failed(reason: string): void; animateSparse?: boolean }): Promise<Surface> {
  const { scale } = options;
  if (!Number.isFinite(scale) || scale <= 0 || !Number.isInteger(target.width / scale) || !Number.isInteger(target.height / scale)) throw Error('nonintegral logical dimensions');
  const win = new BrowserWindow({ show: false, focusable: false, width: target.width / scale, height: target.height / scale,
    transparent: true, backgroundColor: '#00000000', webPreferences: { offscreen: { useSharedTexture: false, deviceScaleFactor: scale },
      backgroundThrottling: false, contextIsolation: true, nodeIntegration: false, sandbox: true } });
  const producer = new WidgetProducer(new ImageSource(options.imageMode, target), bridge, target.instance, options.copyMode);
  let closed = false, generation = 0;
  const pending = new Set<() => void>();
  const cancel = (): void => { for (const stop of pending) stop(); pending.clear(); };
  const invalidate = (next: number): void => {
    if (closed || next < generation) return;
    if (next > generation) cancel();
    generation = next;
    void win.webContents.executeJavaScript(`window.underwolfGeneration=${next};window.underwolfResume?.()`).catch(error => { if (!closed) options.failed(String(error)); });
  };
  win.webContents.setFrameRate(60);
  win.webContents.on('paint', (_event, dirty, image) => { if (!closed) { try { producer.onPaint(image, dirty); } catch (e) { options.failed(String(e)); } } });
  win.webContents.on('render-process-gone', (_event, details) => { if (!closed) options.failed(`renderer lost: ${details.reason}`); });
  const close = (): void => { if (!closed) { closed = true; cancel(); try { producer.close(); } finally { win.destroy(); } } };
  try { await win.loadFile(resolve(__dirname, `../widgets/${target.page}.html`), { query: { auto: options.animateSparse ? '1' : '0' } }); }
  catch (e) { close(); throw e; }
  return { win, producer, invalidate, close, async apply(event, application): Promise<void> {
    if (!application.current()) return;
    if (closed) throw Error('current input target was removed');
    let value = structuredClone(event);
    if (value.type === 'record') {
      if (!value.record.target) return;
      if (value.record.target.id !== target.instance.id || value.record.target.generation !== target.instance.generation) return;
      const record = value.record;
      if (record.input?.kind === 3 && value.widget) value.local = localPoint(value.widget, record.input.a, record.input.b, scale);
    }
    if (value.type === 'table') value.config.widgets = value.config.widgets.filter(w => w.instance.id === target.instance.id && w.instance.generation === target.instance.generation);
    if (application.generation > generation) invalidate(application.generation);
    let stop!: () => void;
    const cancelled = new Promise<false>(r => { stop = () => r(false); pending.add(stop); });
    try {
      // Destroying a renderer does not reliably settle an outstanding JS Promise.
      // Invalidation releases the host barrier without treating it as consumption.
      const applied = await Promise.race([win.webContents.executeJavaScript(applySurface(value, application.generation)), cancelled]);
      if (application.current() && (closed || applied !== true)) throw Error('renderer did not consume current application');
    } catch (error) { if (application.current()) throw error; }
    finally { pending.delete(stop); }
  } };
}
