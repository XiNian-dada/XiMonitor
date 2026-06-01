import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import { makeNode } from '@/api/__fixtures__/nodes';
import { useNodesStore } from './nodes';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: {
      ...actual.apiClient,
      listNodes: vi.fn(),
    },
  };
});

const mockListNodes = vi.mocked(apiClient.listNodes);

describe('useNodesStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockListNodes.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('populates nodes on success', async () => {
    const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
    const b = makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'b', tags: [] } });
    mockListNodes.mockResolvedValueOnce([a, b]);
    const store = useNodesStore();

    await store.refresh();
    expect(store.nodes).toEqual([a, b]);
    expect(store.error).toBeNull();
  });

  it('captures non-abort errors', async () => {
    mockListNodes.mockRejectedValueOnce(new ApiError(503, 'down'));
    const store = useNodesStore();

    await store.refresh();
    expect(store.nodes).toEqual([]);
    expect(store.error).toBeInstanceOf(ApiError);
  });

  it('treats ApiAbortError as silent (redirect in flight)', async () => {
    mockListNodes.mockRejectedValueOnce(new ApiAbortError('redirect'));
    const store = useNodesStore();

    await store.refresh();
    expect(store.error).toBeNull();
  });

  it('skips concurrent refresh() calls', async () => {
    let resolve: (v: never[]) => void = () => {};
    mockListNodes.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const store = useNodesStore();

    const first = store.refresh();
    void store.refresh();
    expect(mockListNodes).toHaveBeenCalledTimes(1);

    resolve([]);
    await first;
    expect(mockListNodes).toHaveBeenCalledTimes(1);
  });

  describe('incremental updates (WS)', () => {
    it('applyServerState replaces the entire Map', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
      const b = makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'b', tags: [] } });

      store.applyServerState([a], '2026-06-01T12:00:00Z');
      expect(store.nodes).toEqual([a]);

      store.applyServerState([b], '2026-06-01T12:01:00Z');
      expect(store.nodes).toEqual([b]);
    });

    it('upsertNode merges into the Map', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
      const b = makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'b', tags: [] } });

      store.applyServerState([a], '2026-06-01T12:00:00Z');
      store.upsertNode(b, '2026-06-01T12:01:00Z');

      expect(store.nodes).toHaveLength(2);
      expect(store.nodes).toContainEqual(a);
      expect(store.nodes).toContainEqual(b);
    });

    it('removeNode deletes from the Map', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
      const b = makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'b', tags: [] } });

      store.applyServerState([a, b], '2026-06-01T12:00:00Z');
      store.removeNode('a', '2026-06-01T12:01:00Z');

      expect(store.nodes).toEqual([b]);
    });

    it('rejects stale applyServerState', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
      const b = makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'b', tags: [] } });

      store.applyServerState([a], '2026-06-01T12:01:00Z');
      store.applyServerState([b], '2026-06-01T12:00:00Z');

      expect(store.nodes).toEqual([a]);
    });

    it('rejects stale upsertNode', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });
      const aUpdated = makeNode({
        identity: { node_id: 'a', node_label: 'A Updated', hostname: 'a', tags: [] },
      });

      store.applyServerState([a], '2026-06-01T12:01:00Z');
      store.upsertNode(aUpdated, '2026-06-01T12:00:00Z');

      expect(store.nodes[0].identity.node_label).toBe('A');
    });

    it('rejects stale removeNode', () => {
      const store = useNodesStore();
      const a = makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'a', tags: [] } });

      store.applyServerState([a], '2026-06-01T12:01:00Z');
      store.removeNode('a', '2026-06-01T12:00:00Z');

      expect(store.nodes).toEqual([a]);
    });
  });
});
