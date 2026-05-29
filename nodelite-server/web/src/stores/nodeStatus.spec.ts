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
});
