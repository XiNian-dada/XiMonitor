import { defineStore } from 'pinia';
import { ref, shallowRef } from 'vue';
import { apiClient, type SettingsResponse } from '@/api';
import { ApiAbortError } from '@/api/client';

/**
 * Global server settings (GET /api/settings) — shared by the Settings and
 * Account pages and the NodeDetail per-node settings tab. Single global
 * resource (no id), so a simple concurrent-load guard suffices.
 */
export const useSettingsStore = defineStore('settings', () => {
  const data = shallowRef<SettingsResponse | null>(null);
  const loading = ref(false);
  const error = ref<Error | null>(null);

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    error.value = null;
    try {
      data.value = await apiClient.settings();
    } catch (e) {
      if (e instanceof ApiAbortError) return;
      error.value = e instanceof Error ? e : new Error(String(e));
    } finally {
      loading.value = false;
    }
  }

  /** Alias for refresh(); pages call this on mount for readability. */
  async function load(): Promise<void> {
    await refresh();
  }

  return { data, loading, error, load, refresh };
});
