import { setActivePinia, createPinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiAbortError, ApiError } from '@/api/client';
import { apiClient } from '@/api';
import { makeOverview } from '@/api/__fixtures__/nodes';
import { useOverviewStore } from './overview';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: {
      ...actual.apiClient,
      overview: vi.fn(),
    },
  };
});

const mockOverview = vi.mocked(apiClient.overview);

describe('useOverviewStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockOverview.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('populates data on success', async () => {
    const overview = makeOverview({ total_nodes: 5 });
    mockOverview.mockResolvedValueOnce(overview);
    const store = useOverviewStore();

    await store.refresh();
    expect(store.data).toEqual(overview);
    expect(store.error).toBeNull();
  });

  it('captures non-abort errors', async () => {
    mockOverview.mockRejectedValueOnce(new ApiError(503, 'down'));
    const store = useOverviewStore();

    await store.refresh();
    expect(store.data).toBeNull();
    expect(store.error).toBeInstanceOf(ApiError);
  });

  it('treats ApiAbortError as silent (redirect in flight)', async () => {
    mockOverview.mockRejectedValueOnce(new ApiAbortError('redirect'));
    const store = useOverviewStore();

    await store.refresh();
    expect(store.error).toBeNull();
  });

  it('skips concurrent refresh() calls', async () => {
    let resolve: (v: ReturnType<typeof makeOverview>) => void = () => {};
    mockOverview.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const store = useOverviewStore();

    const first = store.refresh();
    void store.refresh();
    expect(mockOverview).toHaveBeenCalledTimes(1);

    resolve(makeOverview());
    await first;
    expect(mockOverview).toHaveBeenCalledTimes(1);
  });
});
