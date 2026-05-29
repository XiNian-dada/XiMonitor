import { defineStore } from 'pinia';
import { reactive } from 'vue';
import { apiClient, type HistoryPoint } from '@/api';
import { ApiAbortError } from '@/api/client';

/** Home-page sparkline window, matching legacy HOME_SPARK_* constants. */
export const SPARK_WINDOW_HOURS = 3;
export const SPARK_MAX_POINTS = 180;
export const SPARK_REFRESH_MS = 60 * 1000;

export interface NodeHistoryEntry {
  points: HistoryPoint[];
  fetchedAt: number;
  loading: boolean;
  error: Error | null;
}

/**
 * Per-node history cache for the dashboard sparklines. NodeCard calls
 * loadIfStale on mount; the store dedups concurrent fetches for the same id
 * (a Set guard — the TTL check alone doesn't help on first paint when every
 * card's fetchedAt is still 0) and skips refetching within the TTL.
 */
export const useNodeHistoryStore = defineStore('nodeHistory', () => {
  const entries = reactive<Record<string, NodeHistoryEntry>>({});
  // Not reactive — purely a concurrency guard so N cards mounting at once
  // don't fire N requests for the same node.
  const pending = new Set<string>();

  function points(nodeId: string): HistoryPoint[] {
    return entries[nodeId]?.points ?? [];
  }

  function ensure(nodeId: string): NodeHistoryEntry {
    const existing = entries[nodeId];
    if (existing) return existing;
    const created: NodeHistoryEntry = {
      points: [],
      fetchedAt: 0,
      loading: false,
      error: null,
    };
    entries[nodeId] = created;
    return created;
  }

  async function fetchNow(nodeId: string): Promise<void> {
    if (pending.has(nodeId)) return;
    pending.add(nodeId);
    const entry = ensure(nodeId);
    entry.loading = true;
    entry.error = null;
    try {
      entry.points = await apiClient.nodeHistory(nodeId, {
        windowHours: SPARK_WINDOW_HOURS,
        maxPoints: SPARK_MAX_POINTS,
      });
      entry.fetchedAt = Date.now();
    } catch (e) {
      if (!(e instanceof ApiAbortError)) {
        entry.error = e instanceof Error ? e : new Error(String(e));
      }
    } finally {
      entry.loading = false;
      pending.delete(nodeId);
    }
  }

  async function loadIfStale(nodeId: string, ttlMs = SPARK_REFRESH_MS): Promise<void> {
    const entry = entries[nodeId];
    if (entry && entry.fetchedAt > 0 && Date.now() - entry.fetchedAt < ttlMs) {
      return;
    }
    await fetchNow(nodeId);
  }

  async function forceReload(nodeId: string): Promise<void> {
    await fetchNow(nodeId);
  }

  return { entries, points, loadIfStale, forceReload };
});
