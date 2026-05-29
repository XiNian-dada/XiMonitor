/**
 * Pure SVG sparkline helpers, ported from the legacy aggregateSeries /
 * sparklineSvg / smoothPath in assets/index.html. No DOM — NodeCard renders
 * the returned path strings inside an <svg>.
 */

import type { HistoryPoint } from '@/api';
import type { NodeStatus } from '@/lib/map/projection';

export const SPARK_BUCKET_MS = 60 * 1000;
const SPARK_W = 200;
const SPARK_H = 60;
const SPARK_PAD_Y = 8;

type NumericHistoryField = {
  [K in keyof HistoryPoint]: HistoryPoint[K] extends number | null ? K : never;
}[keyof HistoryPoint];

/**
 * Bucket history points by minute and average a numeric field. Skips
 * null/non-finite values. Returns an ascending-time array of averages.
 */
export function aggregateSeries(
  history: HistoryPoint[] | undefined,
  field: NumericHistoryField,
  bucketMs = SPARK_BUCKET_MS,
): number[] {
  if (!Array.isArray(history) || history.length === 0) return [];
  const buckets = new Map<number, { sum: number; count: number }>();
  for (const point of history) {
    const rawValue = point[field];
    if (rawValue == null) continue;
    const value = Number(rawValue);
    if (!Number.isFinite(value)) continue;
    const ts = Date.parse(point.recorded_at);
    if (!Number.isFinite(ts)) continue;
    const bucket = Math.floor(ts / bucketMs) * bucketMs;
    const item = buckets.get(bucket) || { sum: 0, count: 0 };
    item.sum += value;
    item.count += 1;
    buckets.set(bucket, item);
  }
  return [...buckets.entries()]
    .sort((left, right) => left[0] - right[0])
    .map(([, item]) => item.sum / item.count);
}

export function sparklineColor(status: NodeStatus): string {
  if (status === 'offline') return '#ef4444';
  if (status === 'latency') return '#eab308';
  return '#22c55e';
}

/** Catmull-Rom-ish smoothing → SVG path `d`. Mirrors legacy smoothPath. */
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

export interface Sparkline {
  width: number;
  height: number;
  line: string;
  area: string;
}

/**
 * Build the line + filled-area paths for a sparkline. Returns null when there
 * are fewer than 2 points (the component renders a flat baseline instead).
 */
export function buildSparkline(points: number[]): Sparkline | null {
  if (!Array.isArray(points) || points.length < 2) return null;
  const w = SPARK_W;
  const h = SPARK_H;
  const padY = SPARK_PAD_Y;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = Math.max(max - min, 1);
  const stepX = w / (points.length - 1);
  const coords = points.map(
    (value, idx): [number, number] => [
      idx * stepX,
      h - padY - ((value - min) / span) * (h - padY * 2),
    ],
  );
  const line = smoothPath(coords);
  const area = `${line} L ${w},${h} L 0,${h} Z`;
  return { width: w, height: h, line, area };
}

/**
 * CPU history for the spark, falling back to the node's current snapshot
 * value when no history is cached yet.
 */
export function nodeSparkPoints(
  history: HistoryPoint[] | undefined,
  currentCpu: number | null | undefined,
): number[] {
  const points = aggregateSeries(history, 'cpu_usage_percent');
  if (points.length > 0) return points;
  if (currentCpu == null) return [];
  const current = Number(currentCpu);
  return Number.isFinite(current) ? [current] : [];
}
