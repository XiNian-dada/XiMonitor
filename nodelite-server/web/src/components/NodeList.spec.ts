import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises, RouterLinkStub } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { useNodesStore } from '@/stores/nodes';
import { makeNode } from '@/api/__fixtures__/nodes';
import NodeList from './NodeList.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return {
    ...actual,
    apiClient: { ...actual.apiClient, nodeHistory: vi.fn().mockResolvedValue([]) },
  };
});

const FAKE_DICT = {
  en: {
    'common.waiting_for_data': 'Waiting for data…',
    'index.node.latency': 'Latency',
    'index.node.load': 'Load',
    'index.node.cpu': 'CPU',
    'common.online': 'Online',
    'common.offline': 'Offline',
    'common.latency_warn': 'High latency',
  },
  'zh-CN': { 'common.waiting_for_data': '等待数据…' },
};

const Stub = defineComponent({ render: () => h('div') });

async function mountList(nodes: ReturnType<typeof makeNode>[]) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const store = useNodesStore();
  store.nodes = nodes;
  const wrapper = mount(NodeList, {
    global: {
      plugins: [pinia, getI18n()],
      stubs: { RouterLink: RouterLinkStub },
    },
  });
  await flushPromises();
  return wrapper;
}

describe('NodeList', () => {
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
    vi.clearAllMocks();
  });

  it('renders one card per node, keyed by node id', async () => {
    const wrapper = await mountList([
      makeNode({ identity: { node_id: 'a', node_label: 'A', hostname: 'ha', tags: [] } }),
      makeNode({ identity: { node_id: 'b', node_label: 'B', hostname: 'hb', tags: [] } }),
      makeNode({ identity: { node_id: 'c', node_label: 'C', hostname: 'hc', tags: [] } }),
    ]);
    expect(wrapper.findAll('[data-test="node-card"]')).toHaveLength(3);
    expect(wrapper.find('[data-test="node-list-empty"]').exists()).toBe(false);
  });

  it('shows the empty state when there are no nodes', async () => {
    const wrapper = await mountList([]);
    expect(wrapper.findAll('[data-test="node-card"]')).toHaveLength(0);
    expect(wrapper.find('[data-test="node-list-empty"]').text()).toBe('Waiting for data…');
  });
});
