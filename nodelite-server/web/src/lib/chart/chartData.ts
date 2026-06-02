/**
 * Pure chart data layer, ported from assets/node.html:2035-2147.
 * Buckets/averages history, computes display bounds (with high-percentile spike
 * clipping), and projects per-metric point series. No DOM.
 */

import type { HistoryPoint } from '@/api';

export interface ChartPoint {
  ts: number;
  value: number | null;
}

export interface ChartBounds {
  actualMin: number;
  actualMax: number;
  displayMin: number;
  displayMax: number;
  clipped: boolean;
}

export interface DisplayBoundsOptions {
  clipSpikes?: boolean;
  minValue?: number;
  maxValue?: number;
}

export function quantile(values: number[], ratio: number): number | null {
  if (!Array.isArray(values) || values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(
    sorted.length - 1,
    Math.max(0, Math.floor((sorted.length - 1) * ratio)),
  );
  return sorted[idx] ?? null;
}

export function chartBounds(values: number[], clipSpikes = false): ChartBounds {
  const actualMin = Math.min(...values);
  const actualMax = Math.max(...values);
  let displayMax = actualMax;
  let clipped = false;
  if (clipSpikes && values.length >= 12) {
    const clippedMax = spikeClipMax(values);
    if (clippedMax != null && clippedMax > actualMin && clippedMax < actualMax) {
      displayMax = clippedMax;
      clipped = true;
    }
  }
  return { actualMin, actualMax, displayMin: actualMin, displayMax, clipped };
}

function spikeClipRatio(sampleCount: number): number {
  return sampleCount < 100 ? 0.95 : 0.98;
}

function robustSpikeClipRatio(sampleCount: number): number {
  return sampleCount < 100 ? 0.9 : 0.95;
}

function spikeClipMax(values: number[]): number | null {
  const primaryMax = quantile(values, spikeClipRatio(values.length));
  const robustMax = quantile(values, robustSpikeClipRatio(values.length));
  if (primaryMax == null || robustMax == null) return primaryMax;
  if (robustMax > 0 && primaryMax > robustMax * 8) {
    return robustMax;
  }
  return primaryMax;
}

function niceCeil(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return value;
  const exponent = 10 ** Math.floor(Math.log10(value));
  const normalized = value / exponent;
  const step = [1, 1.25, 2, 2.5, 4, 5, 8, 10].find((candidate) => normalized <= candidate) ?? 10;
  return step * exponent;
}

export function chartDisplayBounds(
  values: number[],
  options: DisplayBoundsOptions = {},
): ChartBounds {
  const bounds = chartBounds(values, options.clipSpikes);
  if (Number.isFinite(Number(options.minValue))) {
    bounds.displayMin = Math.min(bounds.displayMin, Number(options.minValue));
  }
  if (Number.isFinite(Number(options.maxValue))) {
    bounds.displayMax = Math.max(bounds.displayMax, Number(options.maxValue));
  }
  if (bounds.displayMax <= bounds.displayMin) {
    bounds.displayMax = bounds.displayMin + 1;
  }
  if (bounds.displayMin <= 0 && bounds.displayMax > 0) {
    bounds.displayMax = niceCeil(bounds.displayMax);
  }
  return bounds;
}

const HOUR_MS = 3600 * 1000;
const DAY_MS = 24 * HOUR_MS;

/** Bucket width chosen by the selected span (node.html:2055). */
export function chartBucketMs(spanMs: number): number {
  if (spanMs <= 6 * HOUR_MS) return 30 * 1000;
  if (spanMs <= DAY_MS) return 60 * 1000;
  if (spanMs <= 3 * DAY_MS) return 5 * 60 * 1000;
  if (spanMs <= 7 * DAY_MS) return 15 * 60 * 1000;
  return 30 * 60 * 1000;
}

interface Acc {
  sum: number;
  count: number;
}
function add(acc: Acc, value: number | null | undefined): void {
  if (value == null) return;
  const n = Number(value);
  if (!Number.isFinite(n)) return;
  acc.sum += n;
  acc.count += 1;
}
function avg(acc: Acc): number | null {
  return acc.count > 0 ? acc.sum / acc.count : null;
}

export interface AggregatedPoint {
  ts: number;
  recorded_at: string;
  cpu_usage_percent: number | null;
  memory_used_percent: number | null;
  rx_bytes_per_sec: number | null;
  tx_bytes_per_sec: number | null;
  latency_ms: number | null;
}

/**
 * Time-bucket + average the history. Default bucket 60s. Parses recorded_at
 * to epoch ms (legacy pre-stored `_ts`); points with unparseable times are
 * skipped.
 */
export function aggregateHistory(
  history: HistoryPoint[] | undefined,
  bucketMs = 60 * 1000,
): AggregatedPoint[] {
  if (!Array.isArray(history) || history.length === 0) return [];
  const buckets = new Map<
    number,
    { ts: number; cpu: Acc; memory: Acc; rx: Acc; tx: Acc; latency: Acc }
  >();
  for (const point of history) {
    const ms = Date.parse(point.recorded_at);
    if (!Number.isFinite(ms)) continue;
    const bucket = Math.floor(ms / bucketMs) * bucketMs;
    const item =
      buckets.get(bucket) ??
      {
        ts: bucket,
        cpu: { sum: 0, count: 0 },
        memory: { sum: 0, count: 0 },
        rx: { sum: 0, count: 0 },
        tx: { sum: 0, count: 0 },
        latency: { sum: 0, count: 0 },
      };
    add(item.cpu, point.cpu_usage_percent);
    add(item.memory, point.memory_used_percent);
    add(item.rx, point.rx_bytes_per_sec);
    add(item.tx, point.tx_bytes_per_sec);
    add(item.latency, point.latency_ms);
    buckets.set(bucket, item);
  }
  return [...buckets.values()]
    .sort((l, r) => l.ts - r.ts)
    .map((item) => ({
      ts: item.ts,
      recorded_at: new Date(item.ts).toISOString(),
      cpu_usage_percent: avg(item.cpu),
      memory_used_percent: avg(item.memory),
      rx_bytes_per_sec: avg(item.rx),
      tx_bytes_per_sec: avg(item.tx),
      latency_ms: avg(item.latency),
    }));
}

type AggregatedField = Exclude<keyof AggregatedPoint, 'ts' | 'recorded_at'>;

export function chartPoints(history: AggregatedPoint[], field: AggregatedField): ChartPoint[] {
  return history.map((p) => ({ ts: p.ts, value: p[field] }));
}

export function averageValue(points: ChartPoint[]): number | null {
  const values = points.map((p) => Number(p.value)).filter((v) => Number.isFinite(v));
  if (values.length === 0) return null;
  return values.reduce((sum, v) => sum + v, 0) / values.length;
}

export interface ChartData {
  chartHistory: AggregatedPoint[];
  cpuPts: ChartPoint[];
  memPts: ChartPoint[];
  dlPts: ChartPoint[];
  upPts: ChartPoint[];
  rttPts: ChartPoint[];
}

export function buildChartData(
  history: HistoryPoint[] | undefined,
  bucketMs = 60 * 1000,
): ChartData {
  const chartHistory = aggregateHistory(history, bucketMs);
  return {
    chartHistory,
    cpuPts: chartPoints(chartHistory, 'cpu_usage_percent'),
    memPts: chartPoints(chartHistory, 'memory_used_percent'),
    dlPts: chartPoints(chartHistory, 'rx_bytes_per_sec'),
    upPts: chartPoints(chartHistory, 'tx_bytes_per_sec'),
    rttPts: chartPoints(chartHistory, 'latency_ms'),
  };
}
