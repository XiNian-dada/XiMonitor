import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import { useNodeHistoryStore, SPARK_REFRESH_MS } from './nodeHistory';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, nodeHistory: vi.fn() },
  };
});

const mockHistory = vi.mocked(apiClient.nodeHistory);

describe('useNodeHistoryStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockHistory.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it('fetches and stores points for a node', async () => {
    mockHistory.mockResolvedValueOnce([]);
    const store = useNodeHistoryStore();
    await store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledWith('a', { windowHours: 3, maxPoints: 180 });
    expect(store.points('a')).toEqual([]);
  });

  it('dedups concurrent loadIfStale for the same node', async () => {
    let resolve: (v: never[]) => void = () => {};
    mockHistory.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const store = useNodeHistoryStore();

    const a = store.loadIfStale('a');
    const b = store.loadIfStale('a'); // in-flight → no second request
    const c = store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledTimes(1);

    resolve([]);
    await Promise.all([a, b, c]);
    expect(mockHistory).toHaveBeenCalledTimes(1);
  });

  it('skips refetch within the TTL but refetches once stale', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mockHistory.mockResolvedValue([]);
    const store = useNodeHistoryStore();

    await store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledTimes(1);

    // within TTL → no refetch
    vi.setSystemTime(1_000_000 + SPARK_REFRESH_MS - 1);
    await store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledTimes(1);

    // past TTL → refetch
    vi.setSystemTime(1_000_000 + SPARK_REFRESH_MS + 1);
    await store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledTimes(2);
  });

  it('records non-abort errors and keeps points empty', async () => {
    mockHistory.mockRejectedValueOnce(new ApiError(500, 'boom'));
    const store = useNodeHistoryStore();
    await store.loadIfStale('a');
    expect(store.entries['a']?.error).toBeInstanceOf(ApiError);
    expect(store.points('a')).toEqual([]);
  });

  it('swallows ApiAbortError silently', async () => {
    mockHistory.mockRejectedValueOnce(new ApiAbortError('redirect'));
    const store = useNodeHistoryStore();
    await store.loadIfStale('a');
    expect(store.entries['a']?.error).toBeNull();
  });

  it('forceReload bypasses the TTL', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(2_000_000);
    mockHistory.mockResolvedValue([]);
    const store = useNodeHistoryStore();

    await store.loadIfStale('a');
    await store.forceReload('a');
    expect(mockHistory).toHaveBeenCalledTimes(2);
  });
});
