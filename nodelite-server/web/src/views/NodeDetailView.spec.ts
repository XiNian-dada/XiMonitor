import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter, type Router } from 'vue-router';
import { createApp, defineComponent, h } from 'vue';

import NodeDetailView from './NodeDetailView.vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { makeNodeStatus } from '@/api/__fixtures__/nodes';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: {
      ...actual.apiClient,
      nodeStatus: vi.fn(),
      nodeHistory: vi.fn().mockResolvedValue([]),
    },
  };
});

const mockStatus = vi.mocked(apiClient.nodeStatus);
const mockHistory = vi.mocked(apiClient.nodeHistory);

const FAKE_DICT = {
  en: {
    'node.tabs.overview': 'Overview',
    'node.tabs.monitor': 'Monitor',
    'node.tabs.network': 'Network',
    'node.tabs.hardware': 'Hardware',
    'node.tabs.logs': 'Logs',
    'node.tabs.settings': 'Settings',
    'node.meta.ip': 'IP: {ip}',
    'node.meta.uptime_days': 'Up {days}d',
    'node.meta.uptime_hours': 'Up {hours}h',
    'node.info.title': 'Server Info',
    'node.info.os': 'OS',
    'node.info.kernel': 'Kernel',
    'node.info.cpu': 'CPU',
    'node.info.memory': 'Memory',
    'node.info.disk': 'Disk',
    'node.info.virtualization': 'Agent',
    'node.info.uptime': 'Uptime',
    'node.info.cores': '{count} Core(s)',
    'node.uptime.days_hours': '{days}d {hours}h {minutes}m',
    'node.uptime.hours_minutes': '{hours}h {minutes}m',
    'node.uptime.minutes': '{minutes}m',
    'node.disk_usage': 'Disk Usage',
    'node.load': 'Load',
    'node.no_disks': 'No disk metrics.',
    'node.disk.device': 'Device',
    'node.disk.mount': 'Mount',
    'node.disk.filesystem': 'FS',
    'node.disk.usage': 'Usage',
    'node.disk.capacity': 'Capacity',
    'node.cpu_usage': 'CPU Usage',
    'node.memory_usage': 'Memory Usage',
    'node.network_traffic': 'Network Traffic',
    'node.latency_history': 'RTT',
    'node.chart.average': 'Avg {value}',
    'node.waiting_history': 'Waiting…',
    'index.node.download': 'Down',
    'index.node.upload': 'Up',
    'common.unknown': 'Unknown',
    'common.unknown_os': 'unknown os',
    'common.not_available': 'n/a',
    'common.online': 'Online',
    'common.offline': 'Offline',
    'common.latency_warn': 'High latency',
    'common.language': 'Language',
    'common.theme_toggle': 'Toggle theme',
    'index.nav.overview': 'Overview',
    'index.nav.settings': 'Settings',
    'index.nav.alerts': 'Alerts',
    'index.nav.account': 'Account',
  },
  'zh-CN': { 'common.online': '在线' },
};

const Stub = defineComponent({ render: () => h('div') });

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'dashboard', component: Stub },
      { path: '/nodes/:id', name: 'node-detail', component: NodeDetailView },
    ],
  });
}

async function mountDetail(id = 'srv-1') {
  const pinia = createPinia();
  setActivePinia(pinia);
  const router = makeRouter();
  await router.push(`/nodes/${id}`);
  await router.isReady();
  const wrapper = mount(NodeDetailView, {
    global: { plugins: [pinia, router, getI18n()] },
  });
  await flushPromises();
  return { wrapper, router };
}

describe('NodeDetailView', () => {
  beforeEach(async () => {
    window.localStorage.clear();
    __resetI18nForTest();
    mockStatus.mockResolvedValue(
      makeNodeStatus({
        identity: { ...makeNodeStatus().identity, node_id: 'srv-1', node_label: 'Server One', tags: ['ip:10.0.0.9', 'region:eu'] },
        online: true,
        latency_ms: 12,
      }),
    );
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
    vi.clearAllMocks();
  });

  it('loads the node status for the route id', async () => {
    await mountDetail('srv-1');
    expect(mockStatus).toHaveBeenCalledWith('srv-1');
  });

  it('renders the identity header from the loaded status', async () => {
    const { wrapper } = await mountDetail('srv-1');
    expect(wrapper.find('[data-test="node-detail-view"]').text()).toContain('Server One');
    expect(wrapper.find('[data-test="node-status-badge"]').classes()).toContain('online');
    expect(wrapper.find('[data-test="node-meta"]').text()).toContain('IP: 10.0.0.9');
    expect(wrapper.find('[data-test="node-meta"]').text()).toContain('eu');
  });

  it('renders the five tabs with settings disabled', async () => {
    const { wrapper } = await mountDetail();
    for (const tab of ['overview', 'monitor', 'network', 'hardware', 'logs']) {
      expect(wrapper.find(`[data-test="tab-${tab}"]`).exists()).toBe(true);
    }
    expect(wrapper.find('[data-test="tab-settings"]').attributes('disabled')).toBeDefined();
  });

  it('defaults to the overview tab and switches via the URL hash', async () => {
    const { wrapper } = await mountDetail();
    expect(wrapper.find('[data-test="node-tab-pane"]').attributes('data-pane')).toBe('overview');
    expect(wrapper.find('[data-test="tab-overview"]').classes()).toContain('active');

    await wrapper.find('[data-test="tab-monitor"]').trigger('click');
    await flushPromises();
    expect(wrapper.find('[data-test="node-tab-pane"]').attributes('data-pane')).toBe('monitor');
    expect(wrapper.find('[data-test="tab-monitor"]').classes()).toContain('active');
  });

  it('renders overview content (info panel + summary + charts) and loads history', async () => {
    const { wrapper } = await mountDetail('srv-1');
    expect(wrapper.find('[data-test="node-info-panel"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-summary-cards"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="overview-charts"]').exists()).toBe(true);
    // overview is a history tab → overview history fetched on mount
    expect(mockHistory).toHaveBeenCalledWith('srv-1', expect.objectContaining({ windowHours: 336 }));
  });

  it('shows the network pane on the network tab', async () => {
    const { wrapper, router } = await mountDetail('srv-1');
    await router.replace({ hash: '#network' });
    await flushPromises();
    expect(wrapper.find('[data-test="network-pane"]').exists()).toBe(true);
  });

  it('shows disks on the hardware tab', async () => {
    const { wrapper, router } = await mountDetail('srv-1');
    await router.replace({ hash: '#hardware' });
    await flushPromises();
    expect(wrapper.find('[data-test="node-disks"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="node-info-panel"]').exists()).toBe(true);
  });
});
