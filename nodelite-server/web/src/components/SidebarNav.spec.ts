import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import SidebarNav from './SidebarNav.vue';

const FAKE_DICT = {
  en: {
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
  },
  'zh-CN': {
    'index.nav.overview': '概览',
    'index.nav.settings': '设置',
    'index.nav.alerts': '告警',
    'index.nav.account': '账户',
  },
};

const Stub = defineComponent({ render: () => h('div') });

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'dashboard', component: Stub },
      { path: '/nodes/:id', name: 'node-detail', component: Stub },
      { path: '/settings', name: 'settings', component: Stub },
      { path: '/account', name: 'account', component: Stub },
    ],
  });
}

describe('SidebarNav', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        json: () => Promise.resolve(FAKE_DICT),
      } as unknown as Response),
    );
    const dummy = createApp(Stub);
    await setupI18n(dummy);
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
  });

  it('renders all four nav buttons', async () => {
    const router = makeRouter();
    await router.push('/');
    await router.isReady();
    const wrapper = mount(SidebarNav, { global: { plugins: [router, getI18n()] } });

    expect(wrapper.find('[data-test="nav-overview"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="nav-settings"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="nav-alerts"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="nav-account"]').exists()).toBe(true);
  });

  it('marks overview active on the root route', async () => {
    const router = makeRouter();
    await router.push('/');
    await router.isReady();
    const wrapper = mount(SidebarNav, { global: { plugins: [router, getI18n()] } });

    expect(wrapper.find('[data-test="nav-overview"]').classes()).toContain('active');
  });

  it('links Settings and marks it active on /settings', async () => {
    const router = makeRouter();
    await router.push('/settings');
    await router.isReady();
    const wrapper = mount(SidebarNav, { global: { plugins: [router, getI18n()] } });

    const settings = wrapper.find('[data-test="nav-settings"]');
    expect(settings.attributes('disabled')).toBeUndefined();
    expect(settings.classes()).toContain('active');
  });

  it('links Account and marks it active on /account', async () => {
    const router = makeRouter();
    await router.push('/account');
    await router.isReady();
    const wrapper = mount(SidebarNav, { global: { plugins: [router, getI18n()] } });

    const account = wrapper.find('[data-test="nav-account"]');
    expect(account.attributes('disabled')).toBeUndefined();
    expect(account.classes()).toContain('active');
  });

  it('still disables the not-yet-built button (alerts)', async () => {
    const router = makeRouter();
    await router.push('/');
    await router.isReady();
    const wrapper = mount(SidebarNav, { global: { plugins: [router, getI18n()] } });

    expect(wrapper.find('[data-test="nav-alerts"]').attributes('disabled')).toBeDefined();
  });
});
