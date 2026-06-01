import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { apiClient, type NodeListItem } from '@/api';
import { ApiAbortError } from '@/api/client';

/**
 * Node list state. Refactored to Map-keyed for O(1) incremental upserts
 * from WebSocket (Stage 3.5b). Polling lifecycle is NOT owned by the store —
 * see composables/usePolling.ts. Stores hold state + refresh() only.
 *
 * Timestamp guard: single global `lastGeneratedAt` protects against stale
 * messages (e.g., a delayed incremental arriving after a fresh InitialState).
 * This is correct because messages share one ordered WS connection. If we
 * ever introduce concurrent channels (Web Worker, per-node sub-channels),
 * revisit this — a single global would silently drop legitimate concurrent
 * updates.
 */
export const useNodesStore = defineStore('nodes', () => {
  const nodesById = ref<Map<string, NodeListItem>>(new Map());
  const lastGeneratedAt = ref<string | null>(null);
  const loading = ref(false);
  const error = ref<Error | null>(null);

  // Computed array for components that iterate (preserves existing API)
  const nodes = computed(() => Array.from(nodesById.value.values()));

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    error.value = null;
    try {
      const result = await apiClient.listNodes();
      applyServerState(result, new Date().toISOString());
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      error.value = e instanceof Error ? e : new Error(String(e));
    } finally {
      loading.value = false;
    }
  }

  // From WS InitialState (full replacement)
  function applyServerState(items: NodeListItem[], generatedAt: string): void {
    if (lastGeneratedAt.value && generatedAt < lastGeneratedAt.value) return;
    const next = new Map<string, NodeListItem>();
    for (const item of items) next.set(item.identity.node_id, item);
    nodesById.value = next;
    lastGeneratedAt.value = generatedAt;
  }

  // From WS NodeUpsert
  function upsertNode(node: NodeListItem, generatedAt: string): void {
    if (lastGeneratedAt.value && generatedAt < lastGeneratedAt.value) return;
    nodesById.value.set(node.identity.node_id, node);
    lastGeneratedAt.value = generatedAt;
  }

  // From WS NodeRemoved
  function removeNode(nodeId: string, generatedAt: string): void {
    if (lastGeneratedAt.value && generatedAt < lastGeneratedAt.value) return;
    nodesById.value.delete(nodeId);
    lastGeneratedAt.value = generatedAt;
  }

  return {
    nodes,
    nodesById,
    lastGeneratedAt,
    loading,
    error,
    refresh,
    applyServerState,
    upsertNode,
    removeNode,
  };
});
