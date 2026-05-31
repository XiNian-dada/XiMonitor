import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { makeSettings } from '@/api/__fixtures__/nodes';
import NodeSettingsPanel from './NodeSettingsPanel.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: {
      ...actual.apiClient,
      settings: vi.fn(),
      refreshNodeToken: vi.fn(),
    },
  };
});

const mockSettings = vi.mocked(apiClient.settings);
const mockRefresh = vi.mocked(apiClient.refreshNodeToken);

const FAKE_DICT = {
  en: {
    'node.settings.token_info': 'Token Info',
    'node.settings.token_status': 'Status',
    'node.settings.token_expires_at': 'Expires at',
    'node.settings.token_never_expires': 'Never expires',
    'node.settings.token_expired': 'Expired',
    'node.settings.token_expires_in_days': '{days} days',
    'node.settings.token_expires_in_hours': '{hours} hours',
    'node.settings.refresh_token': 'Refresh Token',
    'node.settings.refresh_note': 'Generate a new token for this node',
    'node.settings.refresh_button': 'Refresh',
    'node.settings.refreshing': 'Refreshing…',
    'node.settings.token_refreshed': 'Token refreshed',
    'node.settings.refresh_failed': 'Refresh failed: {error}',
    'common.waiting_for_data': 'Waiting…',
    'settings.password.current': 'Current password',
    'settings.security.verification_code': 'Code',
  },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

describe('NodeSettingsPanel', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    mockSettings.mockResolvedValue(
      makeSettings({
        agents: [
          {
            node_id: 'node-a',
            node_label: 'Node A',
            online: true,
            agent_version: '1.0.0',
            remote_ip: '10.0.0.1',
            tags: [],
            token_expires_at: '2026-06-15T00:00:00Z',
            token_expires_in_secs: 1296000, // 15 days
          },
          {
            node_id: 'node-b',
            node_label: 'Node B',
            online: false,
            agent_version: null,
            remote_ip: null,
            tags: [],
            token_expires_at: null,
            token_expires_in_secs: null,
          },
        ],
      }),
    );
    mockRefresh.mockResolvedValue({
      ok: true,
      message: 'Token refreshed successfully',
      token_expires_at: '2026-07-01T00:00:00Z',
      token_expires_in_secs: 2592000,
    });
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: () => Promise.resolve(FAKE_DICT),
      } as unknown as Response),
    );
    await setupI18n(createApp(Stub));
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mountPanel(nodeId: string) {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useSettingsStore } = await import('@/stores/settings');
    const store = useSettingsStore();
    await store.load();
    const wrapper = mount(NodeSettingsPanel, {
      props: { nodeId },
      global: { plugins: [pinia, getI18n()] },
    });
    await flushPromises();
    return wrapper;
  }

  it('renders token info for the matched node', async () => {
    const wrapper = await mountPanel('node-a');
    expect(wrapper.find('[data-test="node-settings-panel"]').exists()).toBe(true);
    const rows = wrapper.findAll('.info-row');
    expect(rows).toHaveLength(2);
    expect(rows[0]?.text()).toContain('Status');
    expect(rows[0]?.text()).toContain('15 days');
    expect(rows[1]?.text()).toContain('Expires at');
  });

  it('shows "never expires" when token_expires_at is null', async () => {
    const wrapper = await mountPanel('node-b');
    const rows = wrapper.findAll('.info-row');
    expect(rows).toHaveLength(1);
    expect(rows[0]?.text()).toContain('Never expires');
  });

  it('refreshes the token with reauth and shows success message', async () => {
    const wrapper = await mountPanel('node-a');
    await wrapper.find('[data-test="reauth-password"]').setValue('hunter2');
    await wrapper.find('[data-test="refresh-token-button"]').trigger('click');
    await flushPromises();

    expect(mockRefresh).toHaveBeenCalledTimes(1);
    expect(mockRefresh.mock.calls[0]?.[0]).toBe('node-a');
    expect(mockRefresh.mock.calls[0]?.[1]).toMatchObject({ current_password: 'hunter2' });
    expect(mockSettings).toHaveBeenCalledTimes(2); // initial load + refresh after success
    expect(wrapper.find('[data-test="settings-message"]').text()).toBe('Token refreshed successfully');
  });

  it('surfaces the server error message when refresh fails', async () => {
    const { ApiError } = await import('@/api/client');
    mockRefresh.mockReset();
    mockRefresh.mockRejectedValueOnce(
      new ApiError(400, JSON.stringify({ ok: false, message: 'invalid password' })),
    );
    const wrapper = await mountPanel('node-a');
    await wrapper.find('[data-test="refresh-token-button"]').trigger('click');
    await flushPromises();

    const msg = wrapper.find('[data-test="settings-message"]');
    expect(msg.classes()).toContain('error');
    expect(msg.text()).toBe('Refresh failed: invalid password');
  });
});
