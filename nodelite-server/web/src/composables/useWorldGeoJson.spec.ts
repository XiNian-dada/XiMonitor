import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  useWorldGeoJson,
  __resetWorldGeoJsonForTest,
} from './useWorldGeoJson';

const FAKE_GEO = { features: [{ geometry: { type: 'Polygon', coordinates: [] } }] };

describe('useWorldGeoJson', () => {
  beforeEach(() => {
    __resetWorldGeoJsonForTest();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('loads and exposes the geojson on success', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: () => Promise.resolve(FAKE_GEO),
      } as unknown as Response),
    );

    const { geojson, error, load } = useWorldGeoJson();
    expect(geojson.value).toBeNull();
    await load();
    expect(geojson.value).toEqual(FAKE_GEO);
    expect(error.value).toBeNull();
  });

  it('leaves geojson null and records the error on failure', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: false, status: 503 } as unknown as Response),
    );

    const { geojson, error, load } = useWorldGeoJson();
    await load();
    expect(geojson.value).toBeNull();
    expect(error.value).toBeInstanceOf(Error);
  });

  it('caches across instances — second load does not refetch', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(FAKE_GEO),
    } as unknown as Response);
    vi.stubGlobal('fetch', fetchMock);

    await useWorldGeoJson().load();
    // a fresh consumer sees the cached value immediately and triggers no fetch
    const second = useWorldGeoJson();
    expect(second.geojson.value).toEqual(FAKE_GEO);
    await second.load();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('retries after a failure (in-flight promise cleared)', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({ ok: false, status: 500 } as unknown as Response)
      .mockResolvedValueOnce({
        ok: true,
        json: () => Promise.resolve(FAKE_GEO),
      } as unknown as Response);
    vi.stubGlobal('fetch', fetchMock);

    const first = useWorldGeoJson();
    await first.load();
    expect(first.error.value).toBeInstanceOf(Error);

    const second = useWorldGeoJson();
    await second.load();
    expect(second.geojson.value).toEqual(FAKE_GEO);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
