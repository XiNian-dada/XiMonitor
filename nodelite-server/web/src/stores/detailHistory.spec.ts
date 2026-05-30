import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import {
  useDetailHistoryStore,
  NODE_HISTORY_REFRESH_MS,
  RETENTION_WINDOW_HOURS,
  OVERVIEW_HISTORY_MAX_POINTS,
} from './detailHistory';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, nodeHistory: vi.fn() },
  };
});

const mockHistory = vi.mocked(apiClient.nodeHistory);

describe('useDetailHistoryStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockHistory.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it('loads overview history with the 14-day window', async () => {
    mockHistory.mockResolvedValueOnce([]);
    const store = useDetailHistoryStore();
    await store.load('a');
    expect(mockHistory).toHaveBeenCalledWith('a', {
      windowHours: RETENTION_WINDOW_HOURS,
      maxPoints: OVERVIEW_HISTORY_MAX_POINTS,
    });
  });

  it('clears stale points when switching nodes', async () => {
    mockHistory.mockResolvedValueOnce([
      { node_id: 'a', recorded_at: '2026-05-29T00:00:00Z', cpu_usage_percent: 1, memory_used_percent: 2, rx_bytes_per_sec: null, tx_bytes_per_sec: null, latency_ms: null, disk_used_percent: null },
    ]);
    const store = useDetailHistoryStore();
    await store.load('a');
    expect(store.points.length).toBe(1);

    let resolve: (v: never[]) => void = () => {};
    mockHistory.mockReturnValueOnce(new Promise((r) => (resolve = r)));
    const pending = store.load('b');
    expect(store.nodeId).toBe('b');
    expect(store.points).toEqual([]);
    resolve([]);
    await pending;
  });

  it('throttles refresh to >=15s but refetches once stale', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mockHistory.mockResolvedValue([]);
    const store = useDetailHistoryStore();

    await store.load('a');
    expect(mockHistory).toHaveBeenCalledTimes(1);

    vi.setSystemTime(1_000_000 + NODE_HISTORY_REFRESH_MS - 1);
    await store.refresh();
    expect(mockHistory).toHaveBeenCalledTimes(1);

    vi.setSystemTime(1_000_000 + NODE_HISTORY_REFRESH_MS + 1);
    await store.refresh();
    expect(mockHistory).toHaveBeenCalledTimes(2);
  });

  it('fetches the new node when switched mid-flight (id-aware guard)', async () => {
    let resolveA: (v: never[]) => void = () => {};
    mockHistory.mockReturnValueOnce(new Promise((r) => (resolveA = r))).mockResolvedValueOnce([]);
    const store = useDetailHistoryStore();
    const a = store.load('a');
    const b = store.load('b');
    expect(mockHistory).toHaveBeenCalledTimes(2);
    await b;
    expect(store.nodeId).toBe('b');
    resolveA([]);
    await a;
  });

  it('records non-abort errors and swallows ApiAbortError', async () => {
    mockHistory.mockRejectedValueOnce(new ApiError(503, 'down'));
    const store = useDetailHistoryStore();
    await store.load('a');
    expect(store.error).toBeInstanceOf(ApiError);

    setActivePinia(createPinia());
    const store2 = useDetailHistoryStore();
    mockHistory.mockRejectedValueOnce(new ApiAbortError('redirect'));
    await store2.load('a');
    expect(store2.error).toBeNull();
  });

  it('refresh is a no-op with no active node', async () => {
    const store = useDetailHistoryStore();
    await store.refresh();
    expect(mockHistory).not.toHaveBeenCalled();
  });

  it('loadIfStale throttles same-node re-entry but fetches on node switch', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(2_000_000);
    mockHistory.mockResolvedValue([]);
    const store = useDetailHistoryStore();

    await store.loadIfStale('a'); // first load
    expect(mockHistory).toHaveBeenCalledTimes(1);

    // same node, within throttle window → no refetch (the parity fix)
    vi.setSystemTime(2_000_000 + NODE_HISTORY_REFRESH_MS - 1);
    await store.loadIfStale('a');
    expect(mockHistory).toHaveBeenCalledTimes(1);

    // different node → always fetches
    await store.loadIfStale('b');
    expect(mockHistory).toHaveBeenCalledTimes(2);
    expect(store.nodeId).toBe('b');

    // same node again but now stale → refetches
    vi.setSystemTime(2_000_000 + NODE_HISTORY_REFRESH_MS * 4);
    await store.loadIfStale('b');
    expect(mockHistory).toHaveBeenCalledTimes(3);
  });
});
