import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { makeSettings } from '@/api/__fixtures__/nodes';
import SettingsView from './SettingsView.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return { ...actual, apiClient: { ...actual.apiClient, settings: vi.fn() } };
});

const mockSettings = vi.mocked(apiClient.settings);

const FAKE_DICT = {
  en: {
    'settings.heading': 'Settings',
    'settings.subtitle': 'sub',
    'common.waiting_for_data': 'Waiting…',
    'common.language': 'Language',
    'common.theme_toggle': 'Toggle theme',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
    'settings.version.title': 'Version',
    'settings.version.current': 'Current',
    'settings.version.repository': 'Repo',
    'settings.version.public_url': 'URL',
    'settings.version.listen': 'Listen',
    'settings.version.check_updates': 'Check',
    'settings.version.open_release': 'Releases',
    'settings.version.manual_update_note_password': 'note',
    'settings.version.manual_update_note_2fa': 'note',
    'settings.version.update_now': 'Update',
    'settings.password.current': 'Current password',
    'settings.security.verification_code': 'code',
    'settings.ops.title': 'Operations',
    'settings.ops.config': 'Config',
    'settings.ops.registry': 'Registry',
    'settings.ops.history': 'History',
    'settings.ops.snapshot': 'Snapshot',
    'settings.ops.server_upgrade': 'srv',
    'settings.ops.agent_upgrade': 'agent',
    'settings.tokens.title': 'Tokens',
    'settings.tokens.empty': 'none',
    'settings.tokens.node': 'Node',
    'settings.tokens.status': 'Status',
    'settings.tokens.agent': 'Agent',
    'settings.tokens.ip': 'IP',
    'settings.tokens.expires_at': 'Expires',
    'settings.tokens.remaining': 'Remaining',
    'settings.token.no_expiry': 'No expiry',
    'settings.token.expired': 'Expired',
    'settings.duration.days_hours': '{days}d {hours}h',
    'settings.duration.minutes': '{minutes}m',
    'common.online': 'Online',
    'common.offline': 'Offline',
    'common.not_available': 'n/a',
  },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: Stub },
      { path: '/nodes/:id', component: Stub },
      { path: '/settings', name: 'settings', component: SettingsView },
    ],
  });
}

describe('SettingsView', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    mockSettings.mockResolvedValue(makeSettings());
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) =>
        Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve(String(url).includes('ui-i18n') ? FAKE_DICT : { tag_name: 'v2.3.0' }),
        } as unknown as Response),
      ),
    );
    const dummy = createApp(Stub);
    await setupI18n(dummy);
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  async function mountView() {
    const pinia = createPinia();
    setActivePinia(pinia);
    const router = makeRouter();
    await router.push('/settings');
    await router.isReady();
    const wrapper = mount(SettingsView, { global: { plugins: [pinia, router, getI18n()] } });
    await flushPromises();
    return wrapper;
  }

  it('loads settings on mount and renders the three cards', async () => {
    const wrapper = await mountView();
    expect(mockSettings).toHaveBeenCalled();
    expect(wrapper.find('[data-test="settings-view"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="server-update-card"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="ops-card"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="token-table"]').exists()).toBe(true);
  });

  it('shows an error message (not an infinite spinner) when the load fails', async () => {
    const { ApiError } = await import('@/api/client');
    mockSettings.mockReset();
    mockSettings.mockRejectedValueOnce(new ApiError(503, 'service unavailable'));
    const wrapper = await mountView();
    expect(wrapper.find('[data-test="settings-loading"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="server-update-card"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="settings-error"]').exists()).toBe(true);
  });
});
