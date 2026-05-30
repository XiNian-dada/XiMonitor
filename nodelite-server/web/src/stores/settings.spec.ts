import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import { makeSettings } from '@/api/__fixtures__/nodes';
import { useSettingsStore } from './settings';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return { ...actual, apiClient: { ...actual.apiClient, settings: vi.fn() } };
});

const mockSettings = vi.mocked(apiClient.settings);

describe('useSettingsStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockSettings.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('loads settings on success', async () => {
    const settings = makeSettings({ server_version: '9.9.9' });
    mockSettings.mockResolvedValueOnce(settings);
    const store = useSettingsStore();
    await store.load();
    expect(store.data).toEqual(settings);
    expect(store.error).toBeNull();
  });

  it('captures non-abort errors', async () => {
    mockSettings.mockRejectedValueOnce(new ApiError(503, 'down'));
    const store = useSettingsStore();
    await store.load();
    expect(store.data).toBeNull();
    expect(store.error).toBeInstanceOf(ApiError);
  });

  it('swallows ApiAbortError', async () => {
    mockSettings.mockRejectedValueOnce(new ApiAbortError('redirect'));
    const store = useSettingsStore();
    await store.load();
    expect(store.error).toBeNull();
  });

  it('skips concurrent loads', async () => {
    let resolve: (v: ReturnType<typeof makeSettings>) => void = () => {};
    mockSettings.mockReturnValueOnce(new Promise((r) => (resolve = r)));
    const store = useSettingsStore();
    const first = store.load();
    void store.load();
    expect(mockSettings).toHaveBeenCalledTimes(1);
    resolve(makeSettings());
    await first;
    expect(mockSettings).toHaveBeenCalledTimes(1);
  });
});
