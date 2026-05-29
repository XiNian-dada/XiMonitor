import { describe, expect, it } from 'vitest';
import type { HistoryPoint } from '@/api';
import {
  aggregateSeries,
  buildSparkline,
  nodeSparkPoints,
  smoothPath,
  sparklineColor,
} from './sparkline';

function point(recorded_at: string, cpu: number | null): HistoryPoint {
  return {
    node_id: 'n',
    recorded_at,
    cpu_usage_percent: cpu,
    memory_used_percent: 0,
    rx_bytes_per_sec: null,
    tx_bytes_per_sec: null,
    latency_ms: null,
    disk_used_percent: null,
  };
}

describe('aggregateSeries', () => {
  it('returns [] for empty/undefined history', () => {
    expect(aggregateSeries(undefined, 'cpu_usage_percent')).toEqual([]);
    expect(aggregateSeries([], 'cpu_usage_percent')).toEqual([]);
  });

  it('averages values within the same minute bucket, ascending by time', () => {
    const series = aggregateSeries(
      [
        point('2026-05-29T00:00:10Z', 10),
        point('2026-05-29T00:00:50Z', 30), // same minute → avg 20
        point('2026-05-29T00:01:05Z', 50), // next minute
      ],
      'cpu_usage_percent',
    );
    expect(series).toEqual([20, 50]);
  });

  it('skips null and non-finite values', () => {
    const series = aggregateSeries(
      [point('2026-05-29T00:00:10Z', null), point('2026-05-29T00:01:00Z', 42)],
      'cpu_usage_percent',
    );
    expect(series).toEqual([42]);
  });
});

describe('sparklineColor', () => {
  it('maps status to colour', () => {
    expect(sparklineColor('offline')).toBe('#ef4444');
    expect(sparklineColor('latency')).toBe('#eab308');
    expect(sparklineColor('online')).toBe('#22c55e');
  });
});

describe('smoothPath', () => {
  it('returns empty string for no coords', () => {
    expect(smoothPath([])).toBe('');
  });

  it('returns a lone moveTo for a single point', () => {
    expect(smoothPath([[3, 4]])).toBe('M3.0,4.0');
  });

  it('emits a cubic segment for multiple points', () => {
    const d = smoothPath([
      [0, 0],
      [10, 10],
    ]);
    expect(d.startsWith('M0.0,0.0')).toBe(true);
    expect(d).toContain(' C');
  });
});

describe('buildSparkline', () => {
  it('returns null for fewer than 2 points', () => {
    expect(buildSparkline([])).toBeNull();
    expect(buildSparkline([5])).toBeNull();
  });

  it('builds line + closed area paths for >=2 points', () => {
    const spark = buildSparkline([0, 50, 100]);
    expect(spark).not.toBeNull();
    expect(spark!.line.startsWith('M')).toBe(true);
    expect(spark!.area.endsWith('Z')).toBe(true);
    expect(spark!.width).toBe(200);
    expect(spark!.height).toBe(60);
  });
});

describe('nodeSparkPoints', () => {
  it('uses aggregated history when available', () => {
    const pts = nodeSparkPoints(
      [point('2026-05-29T00:00:00Z', 10), point('2026-05-29T00:01:00Z', 20)],
      99,
    );
    expect(pts).toEqual([10, 20]);
  });

  it('falls back to the current snapshot cpu when history is empty', () => {
    expect(nodeSparkPoints([], 42)).toEqual([42]);
    expect(nodeSparkPoints(undefined, 42)).toEqual([42]);
  });

  it('returns [] when neither history nor current cpu is available', () => {
    expect(nodeSparkPoints([], null)).toEqual([]);
    expect(nodeSparkPoints(undefined, undefined)).toEqual([]);
  });
});
