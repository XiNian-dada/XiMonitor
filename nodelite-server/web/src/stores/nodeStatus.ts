import { defineStore } from 'pinia';
import { ref, shallowRef } from 'vue';
import { apiClient, type NodeStatus } from '@/api';
import { ApiAbortError } from '@/api/client';

/**
 * Active node's full status (GET /api/nodes/{id}). Single active node — the
 * NodeDetail view drives load(id) on mount and refresh(id) on each poll.
 * If the id changes (navigating between nodes), data is cleared so a stale
 * node's snapshot never flashes under the new id.
 */
export const useNodeStatusStore = defineStore('nodeStatus', () => {
  const nodeId = ref<string | null>(null);
  const data = shallowRef<NodeStatus | null>(null);
  const loading = ref(false);
  const error = ref<Error | null>(null);

  async function fetchFor(id: string): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    error.value = null;
    try {
      const result = await apiClient.nodeStatus(id);
      // Guard against a late response for a node we've since navigated away from.
      if (nodeId.value === id) data.value = result;
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      error.value = e instanceof Error ? e : new Error(String(e));
    } finally {
      loading.value = false;
    }
  }

  /** Switch to a node: clears stale data if the id changed, then fetches. */
  async function load(id: string): Promise<void> {
    if (nodeId.value !== id) {
      nodeId.value = id;
      data.value = null;
      error.value = null;
    }
    await fetchFor(id);
  }

  /** Re-fetch the current node (polling). No-op if no node is active. */
  async function refresh(): Promise<void> {
    if (nodeId.value === null) return;
    await fetchFor(nodeId.value);
  }

  return { nodeId, data, loading, error, load, refresh };
});
