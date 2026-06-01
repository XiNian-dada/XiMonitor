import { defineStore } from 'pinia';
import { ref, shallowRef } from 'vue';
import { apiClient, type OverviewData } from '@/api';
import { ApiAbortError } from '@/api/client';

/**
 * Overview aggregate stats. Polling lifecycle is NOT owned by the store —
 * see composables/usePolling.ts. Stores hold state + refresh() only.
 *
 * Timestamp guard: single global `lastGeneratedAt` protects against stale
 * messages (e.g., a delayed update arriving after a fresh InitialState).
 */
export const useOverviewStore = defineStore('overview', () => {
  const data = shallowRef<OverviewData | null>(null);
  const lastGeneratedAt = ref<string | null>(null);
  const loading = ref(false);
  const error = ref<Error | null>(null);

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    error.value = null;
    try {
      const result = await apiClient.overview();
      apply(result, new Date().toISOString());
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      error.value = e instanceof Error ? e : new Error(String(e));
    } finally {
      loading.value = false;
    }
  }

  // From WS InitialState or OverviewUpdate
  function apply(overview: OverviewData, generatedAt: string): void {
    if (lastGeneratedAt.value && generatedAt < lastGeneratedAt.value) return;
    data.value = overview;
    lastGeneratedAt.value = generatedAt;
  }

  return { data, lastGeneratedAt, loading, error, refresh, apply };
});
