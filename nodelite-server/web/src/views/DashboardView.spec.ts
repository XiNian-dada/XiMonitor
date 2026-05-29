import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';

import DashboardView from './DashboardView.vue';
import { setupI18n, getI18n, __resetI18nForTest, LANGUAGE_STORAGE_KEY } from '@/i18n';
import { __resetWorldGeoJsonForTest } from '@/composables/useWorldGeoJson';

const FAKE_DICT = {
  en: {
    'index.heading': 'Overview',
    'index.subtitle': 'Global server monitoring · {count} online',
    'common.theme_toggle': 'Toggle theme',
    'common.language': 'Language',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
    'index.stat.total': 'Total Servers',
    'index.stat.online': 'Online',
    'index.stat.offline': 'Offline',
    'index.stat.latency': 'Avg Latency',
  },
  'zh-CN': {
    'index.heading': '概览',
    'index.subtitle': '全球服务器监控 · {count} 在线',
    'common.theme_toggle': '切换主题',
    'common.language': '语言',
    'index.nav.overview': '概览',
    'index.nav.settings': '设置',
    'index.nav.alerts': '告警',
    'index.nav.account': '账户',
    'index.stat.total': '服务器总数',
    'index.stat.online': '在线',
    'index.stat.offline': '离线',
    'index.stat.latency': '平均延迟',
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

async function mountDashboard() {
  const pinia = createPinia();
  setActivePinia(pinia);
  const router = makeRouter();
  await router.push('/');
  await router.isReady();
  const wrapper = mount(DashboardView, {
    global: { plugins: [pinia, router, getI18n()] },
  });
  await flushPromises();
  return wrapper;
}

describe('DashboardView', () => {
  beforeEach(async () => {
    window.localStorage.clear();
    __resetI18nForTest();
    __resetWorldGeoJsonForTest();
    // jsdom has no canvas 2D context; NodeMap's paint no-ops with null.
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
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
    __resetWorldGeoJsonForTest();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    delete document.documentElement.dataset.theme;
  });

  it('renders the shell with sidebar, header, map, and stats', async () => {
    const wrapper = await mountDashboard();
    expect(wrapper.find('[data-test="app-shell"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="dashboard-view"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="sidebar-nav"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-map"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="overview-stats"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-list"]').exists()).toBe(true);
  });

  it('theme toggle flips the html data-theme attribute', async () => {
    document.documentElement.dataset.theme = 'dark';
    const wrapper = await mountDashboard();

    await wrapper.find('[data-test="theme-toggle"]').trigger('click');
    expect(document.documentElement.dataset.theme).toBe('light');

    await wrapper.find('[data-test="theme-toggle"]').trigger('click');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('language select writes the chosen locale to localStorage', async () => {
    const wrapper = await mountDashboard();
    const select = wrapper.find('[data-test="language-select"]');

    await select.setValue('zh-CN');
    expect(window.localStorage.getItem(LANGUAGE_STORAGE_KEY)).toBe('zh-CN');
  });
});
