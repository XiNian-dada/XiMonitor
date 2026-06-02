/**
 * Pure SVG chart-model builders, ported from drawAreaChart (node.html:2350)
 * and drawMultiAreaChart (:2404). Instead of producing innerHTML strings,
 * these return a structured ChartModel (paths, grid, avg lines, hover
 * coords) that MetricChart.vue renders declaratively. No DOM.
 */

import {
  averageValue,
  chartDisplayBounds,
  type ChartBounds,
  type ChartData,
  type ChartPoint,
} from './chartData';
import { formatChartValue, type ChartValueKind } from './format';
import { chartPadLeft, chartY, smoothPath } from './geometry';

const PAD_RIGHT = 14;
const PAD_TOP = 12;
const PAD_BOTTOM = 32;

export interface HoverPoint {
  x: number;
  y: number;
  value: number;
  ts: number | null;
}

export interface ChartGridLine {
  y: number;
  label: string;
}

export interface ChartTimeTick {
  x: number;
  ts: number;
}

export interface ChartSeriesModel {
  label: string;
  color: string;
  kind: ChartValueKind;
  line: string;
  /** Filled-area path; present only for single-series area charts. */
  area?: string;
  /** Dashed average-line y, or null when there's no finite average. */
  avgY: number | null;
  avgOpacity: number;
  points: HoverPoint[];
}

export interface ChartModel {
  width: number;
  height: number;
  padLeft: number;
  padRight: number;
  padTop: number;
  padBottom: number;
  grid: ChartGridLine[];
  timeTicks: ChartTimeTick[];
  series: ChartSeriesModel[];
  /** True when there's no numeric data to plot (render a placeholder). */
  empty: boolean;
}

export interface ChartOptions {
  width: number;
  height: number;
  valueKind?: ChartValueKind;
  clipSpikes?: boolean;
  minValue?: number;
  maxValue?: number;
  padLeft?: number;
}

export interface AreaChartOptions extends ChartOptions {
  color: string;
  label?: string;
}

export interface MultiSeriesInput {
  label: string;
  color: string;
  points: ChartPoint[];
}

/** Network down/up series for a multi-area chart, with the standard colors. */
export function networkSeries(
  data: ChartData,
  downLabel: string,
  upLabel: string,
): MultiSeriesInput[] {
  return [
    { label: downLabel, color: 'var(--chart-network-down)', points: data.dlPts },
    { label: upLabel, color: 'var(--chart-network-up)', points: data.upPts },
  ];
}

function isFiniteValue(p: ChartPoint): p is ChartPoint & { value: number } {
  return p.value != null && Number.isFinite(Number(p.value));
}

// Grid lines span padLeft..width-padRight horizontally; that x-range is
// applied in the template, so only y + label are computed here.
function buildGrid(bounds: ChartBounds, height: number, kind: ChartValueKind): ChartGridLine[] {
  const ratios = height < 100 ? [0, 0.5, 1] : [0, 0.25, 0.5, 0.75, 1];
  return ratios.map((ratio) => {
    const tick = bounds.displayMin + (bounds.displayMax - bounds.displayMin) * ratio;
    return { y: chartY(tick, bounds, height, PAD_TOP, PAD_BOTTOM), label: formatChartValue(tick, kind) };
  });
}

function emptyModel(opts: ChartOptions, padLeft: number): ChartModel {
  return {
    width: opts.width,
    height: opts.height,
    padLeft,
    padRight: PAD_RIGHT,
    padTop: PAD_TOP,
    padBottom: PAD_BOTTOM,
    grid: [],
    timeTicks: [],
    series: [],
    empty: true,
  };
}

function buildTimeTicks(points: HoverPoint[], width: number, padLeft: number): ChartTimeTick[] {
  const withTime = points.filter((p) => p.ts != null);
  if (withTime.length === 0) return [];

  const plotWidth = Math.max(1, width - padLeft - PAD_RIGHT);
  const targetSpacing = 140;
  const tickCount = Math.min(8, Math.max(2, Math.round(plotWidth / targetSpacing) + 1));
  const lastIdx = withTime.length - 1;
  const ticks: ChartTimeTick[] = [];
  const seen = new Set<number>();

  for (let i = 0; i < tickCount; i += 1) {
    const ratio = tickCount === 1 ? 0 : i / (tickCount - 1);
    const idx = Math.round(ratio * lastIdx);
    const point = withTime[idx];
    if (!point || point.ts == null || seen.has(point.ts)) continue;
    seen.add(point.ts);
    ticks.push({ x: point.x, ts: point.ts });
  }

  return ticks;
}

function longestTimedSeries(series: ChartSeriesModel[]): HoverPoint[] {
  return series.reduce<HoverPoint[]>((best, current) => {
    const timed = current.points.filter((p) => p.ts != null);
    return timed.length > best.length ? timed : best;
  }, []);
}

export function buildAreaChart(points: ChartPoint[], opts: AreaChartOptions): ChartModel {
  const kind = opts.valueKind ?? 'number';
  const padLeft = opts.padLeft ?? chartPadLeft(kind, opts.width);
  const numeric = (points ?? []).filter(isFiniteValue);
  if (numeric.length === 0) return emptyModel(opts, padLeft);

  const { width, height } = opts;
  const values = numeric.map((p) => p.value);
  const bounds = chartDisplayBounds(values, opts);
  const plotWidth = width - padLeft - PAD_RIGHT;

  const coords: HoverPoint[] = numeric.map((point, idx) => ({
    x: padLeft + (plotWidth * idx) / Math.max(numeric.length - 1, 1),
    y: chartY(point.value, bounds, height, PAD_TOP, PAD_BOTTOM),
    value: point.value,
    ts: point.ts,
  }));
  const line = smoothPath(coords.map((p) => [p.x, p.y]));
  const area = `${line}L ${width - PAD_RIGHT},${height - PAD_BOTTOM} L ${padLeft},${height - PAD_BOTTOM} Z`;
  const avg = averageValue(numeric);

  return {
    width,
    height,
    padLeft,
    padRight: PAD_RIGHT,
    padTop: PAD_TOP,
    padBottom: PAD_BOTTOM,
    grid: buildGrid(bounds, height, kind),
    timeTicks: buildTimeTicks(coords, width, padLeft),
    series: [
      {
        label: opts.label ?? '',
        color: opts.color,
        kind,
        line,
        area,
        avgY: avg == null ? null : chartY(avg, bounds, height, PAD_TOP, PAD_BOTTOM),
        avgOpacity: 0.36,
        points: coords,
      },
    ],
    empty: false,
  };
}

export function buildMultiAreaChart(series: MultiSeriesInput[], opts: ChartOptions): ChartModel {
  const kind = opts.valueKind ?? 'number';
  const padLeft = opts.padLeft ?? chartPadLeft(kind, opts.width);
  const valid = (series ?? []).filter((s) => Array.isArray(s.points) && s.points.length > 0);
  if (valid.length === 0) return emptyModel(opts, padLeft);

  const allValues = valid.flatMap((s) => s.points.filter(isFiniteValue).map((p) => p.value));
  if (allValues.length === 0) return emptyModel(opts, padLeft);

  const { width, height } = opts;
  const bounds = chartDisplayBounds(allValues, opts);
  const plotWidth = width - padLeft - PAD_RIGHT;
  const longest = Math.max(...valid.map((s) => s.points.length));

  const built: ChartSeriesModel[] = valid.map((s) => {
    const coords: HoverPoint[] = [];
    s.points.forEach((point, idx) => {
      if (!isFiniteValue(point)) return;
      coords.push({
        x: padLeft + (plotWidth * idx) / Math.max(longest - 1, 1),
        y: chartY(point.value, bounds, height, PAD_TOP, PAD_BOTTOM),
        value: point.value,
        ts: point.ts,
      });
    });
    const avg = averageValue(s.points);
    return {
      label: s.label,
      color: s.color,
      kind,
      line: smoothPath(coords.map((p) => [p.x, p.y])),
      avgY: avg == null ? null : chartY(avg, bounds, height, PAD_TOP, PAD_BOTTOM),
      avgOpacity: 0.3,
      points: coords,
    };
  });

  const timeTicks = buildTimeTicks(longestTimedSeries(built), width, padLeft);

  return {
    width,
    height,
    padLeft,
    padRight: PAD_RIGHT,
    padTop: PAD_TOP,
    padBottom: PAD_BOTTOM,
    grid: buildGrid(bounds, height, kind),
    timeTicks,
    series: built,
    empty: false,
  };
}
