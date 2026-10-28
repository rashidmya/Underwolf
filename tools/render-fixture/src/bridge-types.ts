import type { PixelPatch } from './image-source';
export type WidgetKey = { id: number; generation: number };
export type FrameKey = { session: string; widget: WidgetKey; allocation: string };
export type BridgeEvent =
  | { kind: 'ready'; session: string }
  | { kind: 'registered' | 'retired'; key: FrameKey }
  | { kind: 'consumed'; key: FrameKey; seq: string }
  | { kind: 'need_baseline'; widget: WidgetKey }
  | { kind: 'swapchain'; generation: string; width: number; height: number; ready: boolean }
  | { kind: 'progress'; swapchainGeneration: string; completedDraw: string; monotonicNs: string }
  | { kind: 'disconnected' | 'unavailable'; reason: string };
export type PublishResult = { status: 'published' | 'busy' | 'stale'; seq: string; bytes: string };
export type BridgeStats = { cpuBytes: string; publications: string; coalescedHints: string; livePixelBytes: string; retiredPixelBytes: string };
export interface Bridge {
  startServer(peer: { pid: number; startTicks: string }): void;
  createWidget(spec: WidgetKey & { width: number; height: number }): FrameKey;
  canPublish(key: FrameKey): boolean;
  publishPatch(patch: PixelPatch & { key: FrameKey; seq: string }): PublishResult;
  setLayout(layout: { widget: WidgetKey; revision: string; x: number; y: number; width: number; height: number; z: number; visible: boolean }): void;
  setDimming(dimming: { revision: string; active: boolean }): void;
  removeWidget(widget: WidgetKey, revision: string): void;
  drainEvents(): BridgeEvent[];
  stats(): BridgeStats;
  monotonicNowNs(): string;
  stop(): void;
}
