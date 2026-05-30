import { defineStore } from 'pinia';
import { ref, shallowRef } from 'vue';
import { apiClient, type HistoryPoint } from '@/api';
import { ApiAbortError } from '@/api/client';

/** Node-detail overview history window, matching legacy node.html:1414-1418. */
export const RETENTION_WINDOW_HOURS = 24 * 14; // 336h / 14 days
export const OVERVIEW_HISTORY_MAX_POINTS = 1440;
export const NODE_HISTORY_REFRESH_MS = 15 * 1000;

/**
 * Overview-history for the active node (the 14-day low-res sample that
 * feeds the detail charts). Single active node; like nodeStatus, switching
 * ids clears stale points and an id-aware in-flight guard dedups same-node
 * polls without swallowing a node switch. Refetch is throttled to >=15s.
 */
export const useDetailHistoryStore = defineStore('detailHistory', () => {
  const nodeId = ref<string | null>(null);
  const points = shallowRef<HistoryPoint[]>([]);
  const loading = ref(false);
  const error = ref<Error | null>(null);
  const fetchedAt = ref(0);
  const inFlightId = ref<string | null>(null);

  async function fetchFor(id: string): Promise<void> {
    if (inFlightId.value === id) return;
    inFlightId.value = id;
    loading.value = true;
    error.value = null;
    try {
      const result = await apiClient.nodeHistory(id, {
        windowHours: RETENTION_WINDOW_HOURS,
        maxPoints: OVERVIEW_HISTORY_MAX_POINTS,
      });
      if (nodeId.value === id) {
        points.value = result;
        fetchedAt.value = Date.now();
      }
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      if (nodeId.value === id) {
        error.value = e instanceof Error ? e : new Error(String(e));
        fetchedAt.value = Date.now();
      }
    } finally {
      if (inFlightId.value === id) {
        inFlightId.value = null;
        loading.value = false;
      }
    }
  }

  function switchTo(id: string): boolean {
    if (nodeId.value === id) return false;
    nodeId.value = id;
    points.value = [];
    error.value = null;
    fetchedAt.value = 0;
    return true;
  }

  /** Switch to a node (clears stale points) and force-fetch its history. */
  async function load(id: string): Promise<void> {
    switchTo(id);
    await fetchFor(id);
  }

  /**
   * Switch-or-throttled load for tab entry: a new node always fetches; the
   * same node only refetches once >=15s stale. Mirrors legacy
   * fetchOverviewHistory, which throttles regardless of caller — so
   * re-entering a history tab within the window doesn't re-pull the 14-day
   * series.
   */
  async function loadIfStale(id: string): Promise<void> {
    if (switchTo(id)) {
      await fetchFor(id);
      return;
    }
    if (fetchedAt.value > 0 && Date.now() - fetchedAt.value < NODE_HISTORY_REFRESH_MS) {
      return;
    }
    await fetchFor(id);
  }

  /** Poll-driven refresh of the current node, throttled to >=15s. */
  async function refresh(): Promise<void> {
    const id = nodeId.value;
    if (id === null) return;
    if (fetchedAt.value > 0 && Date.now() - fetchedAt.value < NODE_HISTORY_REFRESH_MS) {
      return;
    }
    await fetchFor(id);
  }

  return { nodeId, points, loading, error, load, loadIfStale, refresh };
});
