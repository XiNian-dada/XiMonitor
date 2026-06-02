import { describe, expect, it } from 'vitest';
import { chartPadLeft, chartY, smoothPath } from './geometry';

describe('chartY', () => {
  const bounds = { displayMin: 0, displayMax: 100 };

  it('maps the max to the top padding edge', () => {
    // height 120, padTop 12, padBottom 20 → plot height 88; max → y = padTop = 12
    expect(chartY(100, bounds, 120, 12, 20)).toBeCloseTo(12, 5);
  });

  it('maps the min to the bottom padding edge', () => {
    expect(chartY(0, bounds, 120, 12, 20)).toBeCloseTo(100, 5); // height - padBottom
  });

  it('clamps out-of-range values into the band', () => {
    expect(chartY(200, bounds, 120, 12, 20)).toBeCloseTo(12, 5);
    expect(chartY(-50, bounds, 120, 12, 20)).toBeCloseTo(100, 5);
  });

  it('guards a zero span', () => {
    const flat = { displayMin: 5, displayMax: 5 };
    expect(Number.isFinite(chartY(5, flat, 120, 12, 20))).toBe(true);
  });
});

describe('chartPadLeft', () => {
  it('keeps enough spacing when width is unknown or roomy', () => {
    expect(chartPadLeft('rate')).toBe(78);
    expect(chartPadLeft('latency')).toBe(70);
    expect(chartPadLeft('percent')).toBe(62);
    expect(chartPadLeft('number')).toBe(62);
  });

  it('uses compact spacing in narrow metric cards', () => {
    expect(chartPadLeft('rate', 380)).toBe(62);
    expect(chartPadLeft('latency', 380)).toBe(58);
    expect(chartPadLeft('percent', 380)).toBe(46);
    expect(chartPadLeft('number', 380)).toBe(46);
  });

  it('uses middle spacing before returning to roomy chart padding', () => {
    expect(chartPadLeft('rate', 460)).toBe(68);
    expect(chartPadLeft('latency', 460)).toBe(62);
    expect(chartPadLeft('percent', 460)).toBe(50);
  });
});

describe('smoothPath', () => {
  it('returns empty for no coords and a moveTo for one', () => {
    expect(smoothPath([])).toBe('');
    expect(smoothPath([[3, 4]])).toBe('M3.0,4.0');
  });

  it('emits cubic segments for multiple coords', () => {
    const d = smoothPath([
      [0, 0],
      [10, 5],
      [20, 0],
    ]);
    expect(d.startsWith('M0.0,0.0')).toBe(true);
    expect((d.match(/ C/g) ?? []).length).toBe(2);
  });
});
