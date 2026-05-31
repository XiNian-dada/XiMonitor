import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { makeSettings } from '@/api/__fixtures__/nodes';
import { AUTH_TIMESTAMP_KEY } from '@/auth/expiry';
import AccountView from './AccountView.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return { ...actual, apiClient: { ...actual.apiClient, settings: vi.fn() } };
});

const mockSettings = vi.mocked(apiClient.settings);

const FAKE_DICT = {
  en: {
    'account.heading': 'Account',
    'account.subtitle': 'sub',
    'common.waiting_for_data': 'Waiting…',
    'common.language': 'Language',
    'common.theme_toggle': 'Toggle theme',
    'common.online': 'Online',
    'common.offline': 'Offline',
    'common.not_available': 'n/a',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
    'settings.security.title': 'Security',
    'settings.security.auth': 'Auth',
    'settings.security.username': 'Username',
    'settings.security.2fa': '2FA',
    'settings.security.2fa_note': 'note',
    'settings.security.session_ttl': 'Session TTL',
    'settings.security.logout': 'Sign out',
    'settings.security.start_2fa': 'Set up',
    'settings.enabled': 'Enabled',
    'settings.disabled': 'Disabled',
    'settings.duration.days_hours': '{days}d {hours}h',
    'settings.duration.minutes': '{minutes}m',
    'settings.password.title': 'Change Password',
    'settings.password.current': 'Current password',
    'settings.password.new': 'New password',
    'settings.password.generate': 'Generate',
    'settings.password.submit': 'Update password',
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
      { path: '/settings', component: Stub },
      { path: '/account', name: 'account', component: AccountView },
    ],
  });
}

describe('AccountView', () => {
  let assignSpy: ReturnType<typeof vi.fn>;
  let originalLocation: Location;

  beforeEach(async () => {
    __resetI18nForTest();
    mockSettings.mockResolvedValue(makeSettings());
    window.localStorage.clear();
    originalLocation = window.location;
    assignSpy = vi.fn();
    Object.defineProperty(window, 'location', {
      configurable: true,
      value: { ...originalLocation, assign: assignSpy },
    });
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(FAKE_DICT) } as unknown as Response),
    );
    const dummy = createApp(Stub);
    await setupI18n(dummy);
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
    Object.defineProperty(window, 'location', { configurable: true, value: originalLocation });
    vi.clearAllMocks();
  });

  async function mountView() {
    const pinia = createPinia();
    setActivePinia(pinia);
    const router = makeRouter();
    await router.push('/account');
    await router.isReady();
    const wrapper = mount(AccountView, { global: { plugins: [pinia, router, getI18n()] } });
    await flushPromises();
    return wrapper;
  }

  it('loads settings on mount and renders security + 2FA + password cards', async () => {
    const wrapper = await mountView();
    expect(mockSettings).toHaveBeenCalled();
    expect(wrapper.find('[data-test="security-card"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="two-factor-panel"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="change-password-card"]').exists()).toBe(true);
  });

  it('logout clears the auth timestamp and navigates to logout-and-reauth', async () => {
    window.localStorage.setItem(AUTH_TIMESTAMP_KEY, '12345');
    const wrapper = await mountView();
    await wrapper.find('[data-test="account-logout"]').trigger('click');
    expect(window.localStorage.getItem(AUTH_TIMESTAMP_KEY)).toBeNull();
    expect(assignSpy).toHaveBeenCalledWith('/logout-and-reauth');
  });

  it('shows an error message (not an infinite spinner) when the load fails', async () => {
    const { ApiError } = await import('@/api/client');
    mockSettings.mockReset();
    mockSettings.mockRejectedValueOnce(new ApiError(503, 'service unavailable'));
    const wrapper = await mountView();
    expect(wrapper.find('[data-test="account-loading"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="security-card"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="account-error"]').exists()).toBe(true);
  });
});
