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
  // Id of the request currently in flight, for same-id dedup. Must be
  // id-aware: a plain `loading` guard would swallow a node *switch* whose
  // fetch arrives while a previous node's request is still pending.
  const inFlightId = ref<string | null>(null);

  async function fetchFor(id: string): Promise<void> {
    // Dedup only same-node concurrent fetches (polling). A different id
    // (node switch) must always fetch, even with a request in flight.
    if (inFlightId.value === id) return;
    inFlightId.value = id;
    loading.value = true;
    error.value = null;
    try {
      const result = await apiClient.nodeStatus(id);
      // Discard a late response for a node we've since navigated away from.
      if (nodeId.value === id) data.value = result;
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      if (nodeId.value === id) {
        error.value = e instanceof Error ? e : new Error(String(e));
      }
    } finally {
      // Only clear if we're still the latest in-flight request (a newer
      // switch may have taken over inFlightId/loading).
      if (inFlightId.value === id) {
        inFlightId.value = null;
        loading.value = false;
      }
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
