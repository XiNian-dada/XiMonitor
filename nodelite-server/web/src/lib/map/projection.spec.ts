import { describe, expect, it } from 'vitest';
import { makeNode } from '@/api/__fixtures__/nodes';
import {
  MAP_HEIGHT,
  MAP_WIDTH,
  clamp01,
  hashString,
  mapProject,
  nodeFlag,
  nodePosition,
  nodeRegionKey,
  nodeStatusKey,
} from './projection';

describe('hashString', () => {
  it('is deterministic and non-negative', () => {
    expect(hashString('node-a')).toBe(hashString('node-a'));
    expect(hashString('node-a')).toBeGreaterThanOrEqual(0);
  });

  it('differs for different inputs', () => {
    expect(hashString('node-a')).not.toBe(hashString('node-b'));
  });
});

describe('clamp01', () => {
  it('clamps to [0.02, 0.98]', () => {
    expect(clamp01(-1)).toBe(0.02);
    expect(clamp01(5)).toBe(0.98);
    expect(clamp01(0.5)).toBe(0.5);
  });
});

describe('nodeRegionKey', () => {
  it('matches an explicit region tag', () => {
    const node = makeNode({
      identity: { node_id: 'x', node_label: 'X', hostname: 'h', tags: ['jp'] },
    });
    expect(nodeRegionKey(node)).toBe('jp');
  });

  it('matches a country:xx style tag', () => {
    const node = makeNode({
      identity: { node_id: 'x', node_label: 'X', hostname: 'h', tags: ['country:de'] },
    });
    expect(nodeRegionKey(node)).toBe('de');
  });

  it('falls back to hostname token match', () => {
    const node = makeNode({
      identity: { node_id: 'x', node_label: 'X', hostname: 'web-us-1', tags: [] },
    });
    expect(nodeRegionKey(node)).toBe('us');
  });

  it('returns null when nothing matches', () => {
    const node = makeNode({
      identity: { node_id: 'zzz', node_label: 'Z', hostname: 'host', tags: [] },
    });
    expect(nodeRegionKey(node)).toBeNull();
  });
});

describe('nodePosition', () => {
  it('is deterministic for the same node id', () => {
    const node = makeNode({
      identity: { node_id: 'srv-1', node_label: 'S', hostname: 'h', tags: ['jp'] },
    });
    expect(nodePosition(node)).toEqual(nodePosition(node));
  });

  it('places region-tagged nodes near the region anchor', () => {
    const node = makeNode({
      identity: { node_id: 'srv-1', node_label: 'S', hostname: 'h', tags: ['jp'] },
    });
    const { x, y } = nodePosition(node);
    // jp anchor is [0.86, 0.42]; jitter is < ±0.02
    expect(x).toBeGreaterThan(0.82);
    expect(x).toBeLessThan(0.9);
    expect(y).toBeGreaterThan(0.38);
    expect(y).toBeLessThan(0.46);
  });

  it('keeps unknown-region nodes within the safe band', () => {
    const node = makeNode({
      identity: { node_id: 'mystery', node_label: 'M', hostname: 'h', tags: [] },
    });
    const { x, y } = nodePosition(node);
    expect(x).toBeGreaterThanOrEqual(0.15);
    expect(x).toBeLessThanOrEqual(0.85);
    expect(y).toBeGreaterThanOrEqual(0.2);
    expect(y).toBeLessThanOrEqual(0.8);
  });
});

describe('nodeStatusKey', () => {
  it('returns offline when not online', () => {
    expect(nodeStatusKey(makeNode({ online: false }))).toBe('offline');
  });

  it('returns latency when online but over the warn threshold', () => {
    expect(nodeStatusKey(makeNode({ online: true, latency_ms: 250 }))).toBe('latency');
  });

  it('returns online when healthy', () => {
    expect(nodeStatusKey(makeNode({ online: true, latency_ms: 20 }))).toBe('online');
  });

  it('returns online when latency is null', () => {
    expect(nodeStatusKey(makeNode({ online: true, latency_ms: null }))).toBe('online');
  });
});

describe('nodeFlag', () => {
  it('returns the region flag when known', () => {
    const node = makeNode({
      identity: { node_id: 'x', node_label: 'X', hostname: 'h', tags: ['us'] },
    });
    expect(nodeFlag(node)).toBe('🇺🇸');
  });

  it('falls back to a globe for unknown regions', () => {
    const node = makeNode({
      identity: { node_id: 'zzz', node_label: 'Z', hostname: 'host', tags: [] },
    });
    expect(nodeFlag(node)).toBe('🌐');
  });
});

describe('mapProject', () => {
  it('maps lon=0 to the horizontal centre', () => {
    expect(mapProject(0, 0).x).toBeCloseTo(MAP_WIDTH / 2, 5);
  });

  it('maps lon=-180 to the left edge and lon=180 to the right edge', () => {
    expect(mapProject(-180, 0).x).toBeCloseTo(0, 5);
    expect(mapProject(180, 0).x).toBeCloseTo(MAP_WIDTH, 5);
  });

  it('places higher latitudes higher on the canvas (smaller y)', () => {
    expect(mapProject(0, 60).y).toBeLessThan(mapProject(0, -20).y);
  });

  it('keeps the equator near vertical mid + shift, within bounds', () => {
    const y = mapProject(0, 0).y;
    expect(y).toBeGreaterThan(0);
    expect(y).toBeLessThan(MAP_HEIGHT);
  });
});
