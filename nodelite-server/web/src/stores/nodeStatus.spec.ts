import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import { makeNodeStatus } from '@/api/__fixtures__/nodes';
import { useNodeStatusStore } from './nodeStatus';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, nodeStatus: vi.fn() },
  };
});

const mockStatus = vi.mocked(apiClient.nodeStatus);

describe('useNodeStatusStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockStatus.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('loads the node status for an id', async () => {
    const status = makeNodeStatus({
      identity: { ...makeNodeStatus().identity, node_id: 'a' },
    });
    mockStatus.mockResolvedValueOnce(status);
    const store = useNodeStatusStore();

    await store.load('a');
    expect(mockStatus).toHaveBeenCalledWith('a');
    expect(store.data).toEqual(status);
    expect(store.nodeId).toBe('a');
  });

  it('clears stale data when switching to a different node', async () => {
    mockStatus.mockResolvedValueOnce(makeNodeStatus());
    const store = useNodeStatusStore();
    await store.load('a');
    expect(store.data).not.toBeNull();

    // Switch to b: data should clear immediately, before the fetch resolves.
    let resolve: (v: ReturnType<typeof makeNodeStatus>) => void = () => {};
    mockStatus.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const pending = store.load('b');
    expect(store.nodeId).toBe('b');
    expect(store.data).toBeNull();
    resolve(makeNodeStatus());
    await pending;
  });

  it('refresh re-fetches the current node', async () => {
    mockStatus.mockResolvedValue(makeNodeStatus());
    const store = useNodeStatusStore();
    await store.load('a');
    await store.refresh();
    expect(mockStatus).toHaveBeenCalledTimes(2);
    expect(mockStatus).toHaveBeenLastCalledWith('a');
  });

  it('refresh is a no-op when no node is active', async () => {
    const store = useNodeStatusStore();
    await store.refresh();
    expect(mockStatus).not.toHaveBeenCalled();
  });

  it('records non-abort errors', async () => {
    mockStatus.mockRejectedValueOnce(new ApiError(404, 'node not found'));
    const store = useNodeStatusStore();
    await store.load('missing');
    expect(store.error).toBeInstanceOf(ApiError);
    expect(store.data).toBeNull();
  });

  it('swallows ApiAbortError silently', async () => {
    mockStatus.mockRejectedValueOnce(new ApiAbortError('redirect'));
    const store = useNodeStatusStore();
    await store.load('a');
    expect(store.error).toBeNull();
  });

  it('fetches the new node when switched while a request is in flight', async () => {
    // a is still pending when we navigate to b. The id-aware guard must NOT
    // swallow b's fetch (the bug: a plain loading guard left data null until
    // the next poll).
    const statusA = makeNodeStatus({
      identity: { ...makeNodeStatus().identity, node_id: 'a' },
    });
    const statusB = makeNodeStatus({
      identity: { ...makeNodeStatus().identity, node_id: 'b' },
    });
    let resolveA: (v: ReturnType<typeof makeNodeStatus>) => void = () => {};
    mockStatus
      .mockReturnValueOnce(
        new Promise((r) => {
          resolveA = r;
        }),
      )
      .mockResolvedValueOnce(statusB);

    const store = useNodeStatusStore();
    const loadA = store.load('a'); // in flight
    const loadB = store.load('b'); // must still fetch b

    expect(mockStatus).toHaveBeenCalledTimes(2);
    expect(mockStatus).toHaveBeenLastCalledWith('b');

    await loadB;
    expect(store.nodeId).toBe('b');
    expect(store.data?.identity.node_id).toBe('b');

    // a's late response is discarded (we're on b now).
    resolveA(statusA);
    await loadA;
    expect(store.data?.identity.node_id).toBe('b');
  });

  it('dedups concurrent fetches for the same node', async () => {
    let resolve: (v: ReturnType<typeof makeNodeStatus>) => void = () => {};
    mockStatus.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const store = useNodeStatusStore();

    const first = store.load('a');
    const second = store.load('a'); // same id, in flight → no second request
    expect(mockStatus).toHaveBeenCalledTimes(1);

    resolve(makeNodeStatus());
    await Promise.all([first, second]);
    expect(mockStatus).toHaveBeenCalledTimes(1);
  });
});
