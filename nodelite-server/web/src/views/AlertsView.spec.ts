import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { makeAlertSettings } from '@/api/__fixtures__/nodes';
import AlertsView from './AlertsView.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, alertSettings: vi.fn(), updateAlertSettings: vi.fn() },
  };
});

const mockLoad = vi.mocked(apiClient.alertSettings);
const mockSave = vi.mocked(apiClient.updateAlertSettings);

const FAKE_DICT = {
  en: {
    'alerts.heading': 'Alerts',
    'alerts.subtitle': 'sub',
    'alerts.save': 'Save',
    'alerts.saving': 'Saving…',
    'alerts.saved': 'Saved',
    'alerts.save_failed': 'Save failed: {error}',
    'alerts.secret.keep': 'leave blank to keep',
    'alerts.rules.title': 'Alert Rules',
    'alerts.rules.add': 'Add rule',
    'alerts.preview.title': 'Preview',
    'settings.password.current': 'Current password',
    'settings.security.verification_code': 'Code',
    'common.waiting_for_data': 'Waiting…',
    'common.language': 'Language',
    'common.theme_toggle': 'Toggle theme',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
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
      { path: '/alerts', name: 'alerts', component: AlertsView },
    ],
  });
}

describe('AlertsView', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    mockLoad.mockResolvedValue(makeAlertSettings());
    mockSave.mockResolvedValue(makeAlertSettings());
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

  async function mountView() {
    const pinia = createPinia();
    setActivePinia(pinia);
    const router = makeRouter();
    await router.push('/alerts');
    await router.isReady();
    const wrapper = mount(AlertsView, { global: { plugins: [pinia, router, getI18n()] } });
    await flushPromises();
    return wrapper;
  }

  it('loads on mount and renders the editor cards', async () => {
    const wrapper = await mountView();
    expect(mockLoad).toHaveBeenCalled();
    expect(wrapper.find('[data-test="alerts-view"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="alert-overview-card"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="alerts-save-bar"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="smtp-host"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="webhook-url"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="inspection-lookback"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="rule-list"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="rule-card"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="preview-card"]').exists()).toBe(true);
  });

  it('shows an error message (not an infinite spinner) when the load fails', async () => {
    const { ApiError } = await import('@/api/client');
    mockLoad.mockReset();
    mockLoad.mockRejectedValueOnce(new ApiError(503, 'service unavailable'));
    const wrapper = await mountView();
    expect(wrapper.find('[data-test="alerts-loading"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="alert-overview-card"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="alerts-error"]').exists()).toBe(true);
  });

  it('saves the draft with typed reauth and shows the saved message', async () => {
    const wrapper = await mountView();
    await wrapper.find('[data-test="reauth-password"]').setValue('hunter2');
    await wrapper.find('[data-test="alerts-save"]').trigger('click');
    await flushPromises();

    expect(mockSave).toHaveBeenCalledTimes(1);
    expect(mockSave.mock.calls[0]?.[0]).toMatchObject({ current_password: 'hunter2' });
    expect(wrapper.find('[data-test="settings-message"]').text()).toBe('Saved');
  });

  it('surfaces the server message when a save is rejected', async () => {
    const { ApiError } = await import('@/api/client');
    mockSave.mockReset();
    mockSave.mockRejectedValueOnce(new ApiError(400, JSON.stringify({ ok: false, message: 'bad code' })));
    const wrapper = await mountView();
    await wrapper.find('[data-test="alerts-save"]').trigger('click');
    await flushPromises();

    const msg = wrapper.find('[data-test="settings-message"]');
    expect(msg.classes()).toContain('error');
    expect(msg.text()).toBe('Save failed: bad code');
  });
});
