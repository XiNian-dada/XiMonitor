import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { apiClient } from './index';

function jsonResponse(body: unknown): Response {
  const headers = new Headers({ 'content-type': 'application/json' });
  return {
    status: 200,
    ok: true,
    redirected: false,
    url: 'http://localhost/x',
    headers,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(JSON.stringify(body)),
  } as unknown as Response;
}

describe('apiClient.nodeHistory', () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue(jsonResponse([]));
    vi.stubGlobal('fetch', fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('builds the history URL with window_hours + max_points', async () => {
    await apiClient.nodeHistory('node-a', { windowHours: 3, maxPoints: 180 });
    expect(fetchMock).toHaveBeenCalledOnce();
    const url = fetchMock.mock.calls[0]![0] as string;
    expect(url).toBe('/api/nodes/node-a/history?window_hours=3&max_points=180');
  });

  it('encodes the node id and omits unset query params', async () => {
    await apiClient.nodeHistory('a b/c');
    const url = fetchMock.mock.calls[0]![0] as string;
    expect(url).toBe('/api/nodes/a%20b%2Fc/history');
  });

  it('sends only the params that are provided', async () => {
    await apiClient.nodeHistory('n', { maxPoints: 60 });
    const url = fetchMock.mock.calls[0]![0] as string;
    expect(url).toBe('/api/nodes/n/history?max_points=60');
  });
});
