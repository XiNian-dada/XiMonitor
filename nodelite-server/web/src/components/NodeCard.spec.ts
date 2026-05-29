import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises, RouterLinkStub } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { useNodeHistoryStore } from '@/stores/nodeHistory';
import { makeNode } from '@/api/__fixtures__/nodes';
import NodeCard from './NodeCard.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, nodeHistory: vi.fn() },
  };
});

const mockHistory = vi.mocked(apiClient.nodeHistory);

const FAKE_DICT = {
  en: {
    'index.node.latency': 'Latency',
    'index.node.load': 'Load',
    'index.node.cpu': 'CPU',
    'common.online': 'Online',
    'common.offline': 'Offline',
    'common.latency_warn': 'High latency',
  },
  'zh-CN': {
    'index.node.latency': '延迟',
    'index.node.load': '负载',
    'index.node.cpu': 'CPU',
    'common.online': '在线',
    'common.offline': '离线',
    'common.latency_warn': '高延迟',
  },
};

const Stub = defineComponent({ render: () => h('div') });

async function mountCard(node: ReturnType<typeof makeNode>) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const wrapper = mount(NodeCard, {
    props: { node },
    global: {
      plugins: [pinia, getI18n()],
      stubs: { RouterLink: RouterLinkStub },
    },
  });
  await flushPromises();
  return wrapper;
}

describe('NodeCard', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    mockHistory.mockResolvedValue([]);
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
    vi.clearAllMocks();
  });

  it('links to the node detail route', async () => {
    const node = makeNode({
      identity: { node_id: 'srv 1', node_label: 'Server', hostname: 'h', tags: [] },
    });
    const wrapper = await mountCard(node);
    const link = wrapper.findComponent(RouterLinkStub);
    expect(link.props('to')).toBe('/nodes/srv%201');
  });

  it('renders metrics with rounded latency, load, and cpu%', async () => {
    const node = makeNode({
      online: true,
      latency_ms: 42,
      snapshot: {
        cpu_usage_percent: 63.7,
        load: { one: 1.234 },
        memory: { total_bytes: 100, used_bytes: 50 },
      },
    });
    const wrapper = await mountCard(node);
    expect(wrapper.find('[data-test="metric-latency"]').text()).toBe('42 ms');
    expect(wrapper.find('[data-test="metric-load"]').text()).toBe('1.23');
    expect(wrapper.find('[data-test="metric-cpu"]').text()).toBe('64%');
    // 63.7% → yellow band [50,80)
    expect(wrapper.find('[data-test="metric-cpu"]').classes()).toContain('accent-yellow');
  });

  it('shows em dashes when metrics are unavailable', async () => {
    const node = makeNode({ online: true, latency_ms: null, snapshot: null });
    const wrapper = await mountCard(node);
    expect(wrapper.find('[data-test="metric-latency"]').text()).toBe('—');
    expect(wrapper.find('[data-test="metric-load"]').text()).toBe('—');
    expect(wrapper.find('[data-test="metric-cpu"]').text()).toBe('—');
  });

  it('shows the offline badge label for an offline node', async () => {
    const node = makeNode({ online: false });
    const wrapper = await mountCard(node);
    const badge = wrapper.find('[data-test="node-badge"]');
    expect(badge.classes()).toContain('offline');
    expect(badge.text()).toBe('Offline');
  });

  it('requests history for its node on mount', async () => {
    const node = makeNode({
      identity: { node_id: 'abc', node_label: 'A', hostname: 'h', tags: [] },
    });
    await mountCard(node);
    expect(mockHistory).toHaveBeenCalledWith('abc', { windowHours: 3, maxPoints: 180 });
  });

  it('re-requests history when the snapshot changes (polling refresh)', async () => {
    // Spy on the store action so the assertion is independent of the TTL
    // clock — the point is the watch fires loadIfStale again, not whether
    // the store decides to refetch.
    const pinia = createPinia();
    setActivePinia(pinia);
    const store = useNodeHistoryStore();
    const loadSpy = vi.spyOn(store, 'loadIfStale');

    const node = makeNode({
      identity: { node_id: 'n1', node_label: 'N', hostname: 'h', tags: [] },
      snapshot: { cpu_usage_percent: 10, load: { one: 0.1 }, memory: { total_bytes: 1, used_bytes: 0 } },
    });
    const wrapper = mount(NodeCard, {
      props: { node },
      global: { plugins: [pinia, getI18n()], stubs: { RouterLink: RouterLinkStub } },
    });
    await flushPromises();
    expect(loadSpy).toHaveBeenCalledTimes(1); // immediate

    // A poll replaces the node object with a fresh snapshot reference.
    await wrapper.setProps({
      node: makeNode({
        identity: { node_id: 'n1', node_label: 'N', hostname: 'h', tags: [] },
        snapshot: { cpu_usage_percent: 55, load: { one: 0.9 }, memory: { total_bytes: 1, used_bytes: 0 } },
      }),
    });
    await flushPromises();
    expect(loadSpy).toHaveBeenCalledTimes(2);
  });
});
