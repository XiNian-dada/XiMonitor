import { describe, expect, it } from 'vitest';
import type { HistoryPoint } from '@/api';
import {
  aggregateHistory,
  averageValue,
  buildChartData,
  chartBounds,
  chartBucketMs,
  chartDisplayBounds,
  chartPoints,
  quantile,
} from './chartData';

function hp(recorded_at: string, over: Partial<HistoryPoint> = {}): HistoryPoint {
  return {
    node_id: 'n',
    recorded_at,
    cpu_usage_percent: null,
    memory_used_percent: 0,
    rx_bytes_per_sec: null,
    tx_bytes_per_sec: null,
    latency_ms: null,
    disk_used_percent: null,
    ...over,
  };
}

describe('quantile', () => {
  it('returns null for empty', () => {
    expect(quantile([], 0.98)).toBeNull();
  });
  it('picks the p98-ish index', () => {
    const vals = Array.from({ length: 100 }, (_, i) => i + 1); // 1..100
    expect(quantile(vals, 0.98)).toBe(98);
  });
  it('does not pick a single max spike in short history windows', () => {
    const vals = [...Array.from({ length: 39 }, (_, i) => i + 1), 1000];
    expect(quantile(vals, 0.98)).toBeLessThan(1000);
  });
});

describe('chartBounds', () => {
  it('uses actual min/max without clipping by default', () => {
    const b = chartBounds([1, 2, 100]);
    expect(b.displayMin).toBe(1);
    expect(b.displayMax).toBe(100);
    expect(b.clipped).toBe(false);
  });

  it('clips the spike at p98 when enabled and >=12 points', () => {
    // 1..50 spread + a single 1000 spike → p98 lands at ~50, between min/max.
    const vals = [...Array.from({ length: 50 }, (_, i) => i + 1), 1000];
    const b = chartBounds(vals, true);
    expect(b.clipped).toBe(true);
    expect(b.displayMax).toBeLessThan(1000);
    expect(b.actualMax).toBe(1000);
  });

  it('does not clip with fewer than 12 points', () => {
    expect(chartBounds([1, 2, 1000], true).clipped).toBe(false);
  });

  it('uses a more robust bound when several adjacent samples are extreme', () => {
    const steady = Array.from({ length: 42 }, (_, i) => 1_000_000 + i);
    const spikes = [800_000_000, 820_000_000, 850_000_000];
    const b = chartBounds([...steady, ...spikes], true);
    expect(b.clipped).toBe(true);
    expect(b.displayMax).toBeLessThan(2_000_000);
  });
});

describe('chartDisplayBounds', () => {
  it('expands to include min/max options and guards zero span', () => {
    const b = chartDisplayBounds([5, 5], { minValue: 0, maxValue: 10 });
    expect(b.displayMin).toBe(0);
    expect(b.displayMax).toBe(10);
  });
  it('forces a 1-unit span when flat', () => {
    const b = chartDisplayBounds([5, 5]);
    expect(b.displayMax).toBe(b.displayMin + 1);
  });
  it('rounds positive zero-based ranges to a readable ceiling', () => {
    const b = chartDisplayBounds([0, 75], { minValue: 0 });
    expect(b.displayMax).toBe(80);
  });
  it('can pin capacity charts to a 100 percent range', () => {
    const b = chartDisplayBounds([74, 76], { minValue: 0, maxValue: 100 });
    expect(b.displayMin).toBe(0);
    expect(b.displayMax).toBe(100);
  });
});

describe('chartBucketMs', () => {
  it('scales bucket width by span', () => {
    expect(chartBucketMs(3 * 3600 * 1000)).toBe(30 * 1000);
    expect(chartBucketMs(20 * 3600 * 1000)).toBe(60 * 1000);
    expect(chartBucketMs(2 * 24 * 3600 * 1000)).toBe(5 * 60 * 1000);
    expect(chartBucketMs(5 * 24 * 3600 * 1000)).toBe(15 * 60 * 1000);
    expect(chartBucketMs(30 * 24 * 3600 * 1000)).toBe(30 * 60 * 1000);
  });
});

describe('aggregateHistory', () => {
  it('returns [] for empty', () => {
    expect(aggregateHistory([])).toEqual([]);
    expect(aggregateHistory(undefined)).toEqual([]);
  });

  it('averages points within the same bucket, ascending, nulls skipped', () => {
    const out = aggregateHistory(
      [
        hp('2026-05-29T00:00:10Z', { cpu_usage_percent: 10 }),
        hp('2026-05-29T00:00:50Z', { cpu_usage_percent: 30 }), // same minute → 20
        hp('2026-05-29T00:01:05Z', { cpu_usage_percent: null }), // skipped → null bucket
      ],
      60 * 1000,
    );
    expect(out).toHaveLength(2);
    expect(out[0]!.cpu_usage_percent).toBe(20);
    expect(out[1]!.cpu_usage_percent).toBeNull();
  });

  it('skips points with unparseable timestamps', () => {
    expect(aggregateHistory([hp('not-a-date', { cpu_usage_percent: 5 })])).toEqual([]);
  });
});

describe('chartPoints / averageValue / buildChartData', () => {
  it('projects a field into {ts,value}', () => {
    const agg = aggregateHistory([hp('2026-05-29T00:00:00Z', { memory_used_percent: 42 })]);
    const pts = chartPoints(agg, 'memory_used_percent');
    expect(pts[0]).toEqual({ ts: agg[0]!.ts, value: 42 });
  });

  it('averages values, counting null as 0 (legacy Number(null)===0 quirk)', () => {
    // node.html:2105 maps value via Number() then filters isFinite; null→0
    // passes, so a null point drags the mean. Kept for parity.
    expect(averageValue([{ ts: 1, value: 10 }, { ts: 2, value: null }, { ts: 3, value: 30 }])).toBe(
      40 / 3,
    );
    expect(averageValue([{ ts: 1, value: null }])).toBe(0);
  });

  it('builds all five metric series', () => {
    const data = buildChartData([
      hp('2026-05-29T00:00:00Z', {
        cpu_usage_percent: 1,
        memory_used_percent: 2,
        rx_bytes_per_sec: 3,
        tx_bytes_per_sec: 4,
        latency_ms: 5,
      }),
    ]);
    expect(data.cpuPts[0]!.value).toBe(1);
    expect(data.memPts[0]!.value).toBe(2);
    expect(data.dlPts[0]!.value).toBe(3);
    expect(data.upPts[0]!.value).toBe(4);
    expect(data.rttPts[0]!.value).toBe(5);
  });
});
