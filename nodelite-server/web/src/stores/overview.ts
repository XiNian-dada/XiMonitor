import { defineStore } from 'pinia';
import { ref, shallowRef } from 'vue';
import { apiClient, type OverviewData } from '@/api';
import { ApiAbortError } from '@/api/client';

/**
 * Overview aggregate stats. Polling lifecycle is NOT owned by the store —
 * see composables/usePolling.ts. Stores hold state + refresh() only.
 */
export const useOverviewStore = defineStore('overview', () => {
  const data = shallowRef<OverviewData | null>(null);
  const loading = ref(false);
  const error = ref<Error | null>(null);

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    error.value = null;
    try {
      data.value = await apiClient.overview();
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      error.value = e instanceof Error ? e : new Error(String(e));
    } finally {
      loading.value = false;
    }
  }

  return { data, loading, error, refresh };
});
