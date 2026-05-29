import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';

import AppLayout from './AppLayout.vue';
import { setupI18n, getI18n, __resetI18nForTest, LANGUAGE_STORAGE_KEY } from '@/i18n';

const FAKE_DICT = {
  en: {
    'common.theme_toggle': 'Toggle theme',
    'common.language': 'Language',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
  },
  'zh-CN': {
    'common.theme_toggle': '切换主题',
    'common.language': '语言',
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
    ],
  });
}

async function mountLayout() {
  const router = makeRouter();
  await router.push('/');
  await router.isReady();
  const wrapper = mount(AppLayout, {
    global: { plugins: [router, getI18n()] },
    slots: {
      title: '<h1 data-test="slot-title">Hi</h1>',
      default: '<div data-test="slot-body">Body</div>',
    },
  });
  await flushPromises();
  return wrapper;
}

describe('AppLayout', () => {
  beforeEach(async () => {
    window.localStorage.clear();
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
    window.localStorage.clear();
    __resetI18nForTest();
    vi.unstubAllGlobals();
    delete document.documentElement.dataset.theme;
  });

  it('renders sidebar, the title slot, and the body slot', async () => {
    const wrapper = await mountLayout();
    expect(wrapper.find('[data-test="app-shell"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="sidebar-nav"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="slot-title"]').text()).toBe('Hi');
    expect(wrapper.find('[data-test="slot-body"]').text()).toBe('Body');
  });

  it('theme toggle flips the html data-theme attribute', async () => {
    document.documentElement.dataset.theme = 'dark';
    const wrapper = await mountLayout();

    await wrapper.find('[data-test="theme-toggle"]').trigger('click');
    expect(document.documentElement.dataset.theme).toBe('light');

    await wrapper.find('[data-test="theme-toggle"]').trigger('click');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('language select writes the chosen locale to localStorage', async () => {
    const wrapper = await mountLayout();
    await wrapper.find('[data-test="language-select"]').setValue('zh-CN');
    expect(window.localStorage.getItem(LANGUAGE_STORAGE_KEY)).toBe('zh-CN');
  });
});
