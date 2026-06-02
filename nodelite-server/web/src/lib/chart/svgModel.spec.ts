import { describe, expect, it } from 'vitest';
import type { ChartPoint } from './chartData';
import { buildAreaChart, buildMultiAreaChart } from './svgModel';

function pts(values: Array<number | null>): ChartPoint[] {
  return values.map((value, i) => ({ ts: i * 60_000, value }));
}

describe('buildAreaChart', () => {
  const opts = { width: 600, height: 200, valueKind: 'percent' as const, color: 'var(--chart-cpu)' };

  it('flags empty when no numeric points', () => {
    expect(buildAreaChart([], opts).empty).toBe(true);
    expect(buildAreaChart(pts([null, null]), opts).empty).toBe(true);
  });

  it('builds one series with a line + area path and grid', () => {
    const model = buildAreaChart(pts([10, 50, 90]), opts);
    expect(model.empty).toBe(false);
    expect(model.series).toHaveLength(1);
    const s = model.series[0]!;
    expect(s.line.startsWith('M')).toBe(true);
    expect(s.area?.endsWith('Z')).toBe(true);
    expect(s.points).toHaveLength(3);
    expect(model.grid.length).toBeGreaterThan(0);
    expect(model.grid[0]!.label).toContain('%');
    expect(model.timeTicks.length).toBeGreaterThanOrEqual(2);
  });

  it('positions the first point at padLeft and last at width-padRight', () => {
    const model = buildAreaChart(pts([10, 90]), opts);
    const s = model.series[0]!;
    expect(s.points[0]!.x).toBeCloseTo(model.padLeft, 5);
    expect(s.points[1]!.x).toBeCloseTo(model.width - model.padRight, 5);
  });

  it('builds time-axis ticks aligned with plotted points', () => {
    const model = buildAreaChart(pts([10, 50, 90]), opts);
    expect(model.timeTicks[0]).toMatchObject({ x: model.padLeft, ts: 0 });
    expect(model.timeTicks.at(-1)).toMatchObject({
      x: model.width - model.padRight,
      ts: 120_000,
    });
  });

  it('uses compact y-axis padding for narrow cards', () => {
    const model = buildAreaChart(pts([10, 90]), { ...opts, width: 380 });
    expect(model.padLeft).toBe(46);
  });

  it('carries ts on hover points', () => {
    const model = buildAreaChart(pts([10, 90]), opts);
    expect(model.series[0]!.points[1]!.ts).toBe(60_000);
  });

  it('uses fewer grid lines for short charts', () => {
    const short = buildAreaChart(pts([10, 90]), { ...opts, height: 70 });
    expect(short.grid).toHaveLength(3);
    const tall = buildAreaChart(pts([10, 90]), opts);
    expect(tall.grid).toHaveLength(5);
  });
});

describe('buildMultiAreaChart', () => {
  const opts = { width: 600, height: 220, valueKind: 'rate' as const };

  it('flags empty when no series have numeric points', () => {
    expect(buildMultiAreaChart([], opts).empty).toBe(true);
    expect(
      buildMultiAreaChart([{ label: 'dl', color: 'c', points: pts([null]) }], opts).empty,
    ).toBe(true);
  });

  it('builds a line per valid series, no area fill, shared bounds', () => {
    const model = buildMultiAreaChart(
      [
        { label: 'down', color: 'var(--chart-network-down)', points: pts([100, 200, 300]) },
        { label: 'up', color: 'var(--chart-network-up)', points: pts([10, 20, 30]) },
      ],
      opts,
    );
    expect(model.empty).toBe(false);
    expect(model.series).toHaveLength(2);
    expect(model.timeTicks.length).toBeGreaterThanOrEqual(2);
    for (const s of model.series) {
      expect(s.line.startsWith('M')).toBe(true);
      expect(s.area).toBeUndefined();
    }
  });

  it('uses compact rate-axis padding for narrow network cards', () => {
    const model = buildMultiAreaChart(
      [
        { label: 'down', color: 'var(--chart-network-down)', points: pts([100, 200, 300]) },
        { label: 'up', color: 'var(--chart-network-up)', points: pts([10, 20, 30]) },
      ],
      { ...opts, width: 380 },
    );
    expect(model.padLeft).toBe(62);
  });
});
