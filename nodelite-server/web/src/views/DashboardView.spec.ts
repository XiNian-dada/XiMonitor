import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';

import DashboardView from './DashboardView.vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { __resetWorldGeoJsonForTest } from '@/composables/useWorldGeoJson';
import { useWebSocket } from '@/ws';
import { useOverviewStore } from '@/stores/overview';
import { useNodesStore } from '@/stores/nodes';
import type { BrowserMessage } from '@/api/types';

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
    'index.matrix.title': 'Latency Overview (ms)',
    'index.matrix.subtitle': 'Recent RTT per node',
    'index.matrix.more': 'More',
    'index.matrix.col_current': 'Now',
    'index.matrix.empty': 'No agents reporting yet.',
    'index.node.cpu': 'CPU',
    'index.node.memory': 'Memory',
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
    'index.matrix.title': '延迟概览 (ms)',
    'index.matrix.subtitle': '节点近期 RTT',
    'index.matrix.more': '更多',
    'index.matrix.col_current': '当前',
    'index.matrix.empty': '暂无节点接入。',
    'index.node.cpu': 'CPU',
    'index.node.memory': '内存',
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

  it('renders inside AppLayout with map, stats, and node list', async () => {
    const wrapper = await mountDashboard();
    // AppLayout chrome (theme/lang coverage lives in AppLayout.spec).
    expect(wrapper.find('[data-test="app-shell"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="sidebar-nav"]').exists()).toBe(true);
    // Dashboard body.
    expect(wrapper.find('[data-test="dashboard-view"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-map"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="overview-stats"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-health-matrix"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-list"]').exists()).toBe(true);
  });

  it('subscribes to WebSocket messages on mount', async () => {
    const ws = useWebSocket();
    const onSpy = vi.spyOn(ws, 'on');

    await mountDashboard();

    expect(onSpy).toHaveBeenCalledWith('initial_state', expect.any(Function));
    expect(onSpy).toHaveBeenCalledWith('overview_update', expect.any(Function));
    expect(onSpy).toHaveBeenCalledWith('node_upsert', expect.any(Function));
    expect(onSpy).toHaveBeenCalledWith('node_removed', expect.any(Function));

    onSpy.mockRestore();
  });

  it('applies InitialState to stores when received via WebSocket', async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const overviewStore = useOverviewStore();
    const nodesStore = useNodesStore();
    const ws = useWebSocket();

    const router = makeRouter();
    await router.push('/');
    await router.isReady();

    mount(DashboardView, {
      global: { plugins: [pinia, router, getI18n()] },
    });

    await flushPromises();

    // Simulate InitialState message
    const msg: BrowserMessage = {
      type: 'initial_state',
      generated_at: '2026-06-01T12:00:00Z',
      overview: {
        generated_at: '2026-06-01T12:00:00Z',
        total_nodes: 5,
        online_nodes: 3,
        offline_nodes: 2,
        total_rx_bytes: 1000,
        total_tx_bytes: 2000,
        current_rx_bytes_per_sec: 10,
        current_tx_bytes_per_sec: 20,
        average_latency_ms: 15,
      },
      nodes: [],
    };

    // Trigger the handler
    const handlers = ws['handlers'].get('initial_state');
    if (handlers) {
      handlers.forEach((handler) => handler(msg));
    }

    expect(overviewStore.data).toEqual(msg.overview);
    expect(nodesStore.lastGeneratedAt).toBe('2026-06-01T12:00:00Z');
  });

  it('falls back to REST if WebSocket does not deliver InitialState within 3s', async () => {
    vi.useFakeTimers();

    const pinia = createPinia();
    setActivePinia(pinia);
    const overviewStore = useOverviewStore();
    const nodesStore = useNodesStore();
    const refreshOverviewSpy = vi.spyOn(overviewStore, 'refresh').mockResolvedValue();
    const refreshNodesSpy = vi.spyOn(nodesStore, 'refresh').mockResolvedValue();

    const router = makeRouter();
    await router.push('/');
    await router.isReady();

    mount(DashboardView, {
      global: { plugins: [pinia, router, getI18n()] },
    });

    await flushPromises();

    // Fast-forward 3s
    vi.advanceTimersByTime(3000);
    await flushPromises();

    expect(refreshOverviewSpy).toHaveBeenCalledTimes(1);
    expect(refreshNodesSpy).toHaveBeenCalledTimes(1);

    vi.useRealTimers();
    refreshOverviewSpy.mockRestore();
    refreshNodesSpy.mockRestore();
  });

  it('does not call REST fallback if WebSocket delivers InitialState in time', async () => {
    vi.useFakeTimers();

    const pinia = createPinia();
    setActivePinia(pinia);
    const overviewStore = useOverviewStore();
    const nodesStore = useNodesStore();
    const refreshOverviewSpy = vi.spyOn(overviewStore, 'refresh').mockResolvedValue();
    const refreshNodesSpy = vi.spyOn(nodesStore, 'refresh').mockResolvedValue();
    const ws = useWebSocket();

    const router = makeRouter();
    await router.push('/');
    await router.isReady();

    mount(DashboardView, {
      global: { plugins: [pinia, router, getI18n()] },
    });

    await flushPromises();

    // Simulate InitialState before 3s timeout
    const msg: BrowserMessage = {
      type: 'initial_state',
      generated_at: '2026-06-01T12:00:00Z',
      overview: {
        generated_at: '2026-06-01T12:00:00Z',
        total_nodes: 5,
        online_nodes: 3,
        offline_nodes: 2,
        total_rx_bytes: 1000,
        total_tx_bytes: 2000,
        current_rx_bytes_per_sec: 10,
        current_tx_bytes_per_sec: 20,
        average_latency_ms: 15,
      },
      nodes: [],
    };

    const handlers = ws['handlers'].get('initial_state');
    if (handlers) {
      handlers.forEach((handler) => handler(msg));
    }

    // Fast-forward 3s
    vi.advanceTimersByTime(3000);
    await flushPromises();

    // REST should NOT be called because WS delivered data
    expect(refreshOverviewSpy).not.toHaveBeenCalled();
    expect(refreshNodesSpy).not.toHaveBeenCalled();

    vi.useRealTimers();
    refreshOverviewSpy.mockRestore();
    refreshNodesSpy.mockRestore();
  });
});
