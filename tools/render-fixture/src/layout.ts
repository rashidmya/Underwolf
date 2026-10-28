import type { WidgetSpec } from './input-consumer';
export type Scenario = 'benchmark' | 'input' | 'small-damage';
export type Target = WidgetSpec & { page: string };
export function layout(scenario: Scenario, bounds: { width: number; height: number }): Target[] {
  const rectangles = scenario === 'benchmark' ? [[20, 20, 400, 300], [460, 20, 300, 200], [460, 240, 300, 200]]
    : scenario === 'input' ? [[100, 100, 340, 240], [300, 240, 300, 200]] : [[32, 32, 256, 192]];
  const fits = rectangles.every(([x, y, width, height]) => x + width <= bounds.width && y + height <= bounds.height);
  return rectangles.map(([x, y, width, height], i) => ({ instance: { id: i + 1, generation: 1 }, x, y, width, height, z: i + 1,
    mode: i === 1 && scenario === 'input' ? 'notify' : 'block', visible: fits, keyboard: true,
    page: scenario === 'benchmark' ? i === 0 ? 'animated' : 'static' : scenario }));
}
