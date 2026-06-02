/**
 * Pure chart geometry, ported from assets/node.html (chartY :2174,
 * chartPadLeft :2198, smoothPath :2330). No DOM.
 */

import type { ChartValueKind } from './format';

export interface ChartBounds {
  displayMin: number;
  displayMax: number;
}

/** Map a value to a y pixel within the padded plot area. */
export function chartY(
  value: number,
  bounds: ChartBounds,
  height: number,
  padTop: number,
  padBottom: number,
): number {
  const span = Math.max(bounds.displayMax - bounds.displayMin, 1);
  const v = Math.min(Math.max(Number(value), bounds.displayMin), bounds.displayMax);
  return height - padBottom - ((v - bounds.displayMin) / span) * (height - padTop - padBottom);
}

/** Left padding by metric kind — compact cards need less whitespace than full-size charts. */
export function chartPadLeft(kind: ChartValueKind, width?: number): number {
  const measuredWidth = Number(width);
  const isCompact = Number.isFinite(measuredWidth) && measuredWidth < 420;
  const isMedium = Number.isFinite(measuredWidth) && measuredWidth < 520;

  if (kind === 'rate') {
    if (isCompact) return 62;
    if (isMedium) return 68;
    return 78;
  }
  if (kind === 'latency') {
    if (isCompact) return 58;
    if (isMedium) return 62;
    return 70;
  }
  if (isCompact) return 46;
  if (isMedium) return 50;
  return 62;
}

/** Catmull-Rom → cubic-Bézier smoothing producing an SVG path `d`. */
export function smoothPath(coords: Array<[number, number]>): string {
  if (!coords.length) return '';
  const first = coords[0]!;
  if (coords.length === 1) {
    return `M${first[0].toFixed(1)},${first[1].toFixed(1)}`;
  }
  let path = `M${first[0].toFixed(1)},${first[1].toFixed(1)}`;
  for (let i = 0; i < coords.length - 1; i++) {
    const p0 = coords[i - 1] || coords[i]!;
    const p1 = coords[i]!;
    const p2 = coords[i + 1]!;
    const p3 = coords[i + 2] || p2;
    const cp1x = p1[0] + (p2[0] - p0[0]) / 6;
    const cp1y = p1[1] + (p2[1] - p0[1]) / 6;
    const cp2x = p2[0] - (p3[0] - p1[0]) / 6;
    const cp2y = p2[1] - (p3[1] - p1[1]) / 6;
    path += ` C${cp1x.toFixed(1)},${cp1y.toFixed(1)} ${cp2x.toFixed(1)},${cp2y.toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
  }
  return path;
}
