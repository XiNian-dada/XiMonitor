import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { makeNode } from '@/api/__fixtures__/nodes';
import { useNodesStore } from '@/stores/nodes';
import NodeHealthMatrix from './NodeHealthMatrix.vue';

const FAKE_DICT = {
  en: {
    'index.matrix.title': 'Latency Overview (ms)',
    'index.matrix.subtitle': 'Recent RTT per node',
    'index.matrix.more': 'More',
    'index.matrix.col_current': 'Now',
    'index.matrix.empty': 'No agents reporting yet.',
    'index.node.load': 'Load',
    'index.node.cpu': 'CPU',
    'index.node.memory': 'Memory',
  },
  'zh-CN': {
    'index.matrix.title': '延迟概览 (ms)',
    'index.matrix.subtitle': '节点近期 RTT',
    'index.matrix.more': '更多',
    'index.matrix.col_current': '当前',
    'index.matrix.empty': '暂无节点接入。',
    'index.node.load': '负载',
    'index.node.cpu': 'CPU',
    'index.node.memory': '内存',
  },
};

const Stub = defineComponent({ render: () => h('div') });

async function mountMatrix(nodes = [] as ReturnType<typeof makeNode>[]) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const store = useNodesStore();
  store.applyServerState(nodes, '2026-06-01T12:00:00Z');

  const wrapper = mount(NodeHealthMatrix, {
    global: { plugins: [pinia, getI18n()] },
  });
  await wrapper.vm.$nextTick();
  return wrapper;
}

describe('NodeHealthMatrix', () => {
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

  it('shows the empty state before agents report', async () => {
    const wrapper = await mountMatrix();
    expect(wrapper.find('[data-test="health-matrix-empty"]').text()).toBe('No agents reporting yet.');
  });

  it('sorts nodes by label and limits the table to ten rows', async () => {
    const names = ['Zulu', 'Alpha', 'Bravo', 'Charlie', 'Delta', 'Echo', 'Foxtrot', 'Golf', 'Hotel', 'India', 'Juliet'];
    const wrapper = await mountMatrix(
      names.map((name) =>
        makeNode({
          identity: { node_id: name.toLowerCase(), node_label: name, hostname: name, tags: [] },
        }),
      ),
    );

    const rows = wrapper.findAll('[data-test="health-matrix-row"]');
    expect(rows).toHaveLength(10);
    expect(rows.map((row) => row.find('.row-head').text())).toEqual([
      'Alpha',
      'Bravo',
      'Charlie',
      'Delta',
      'Echo',
      'Foxtrot',
      'Golf',
      'Hotel',
      'India',
      'Juliet',
    ]);
  });

  it('renders latency, load, cpu, and memory values with legacy tones', async () => {
    const wrapper = await mountMatrix([
      makeNode({
        identity: { node_id: 'alpha', node_label: 'Alpha', hostname: 'alpha', tags: [] },
        latency_ms: 42.4,
        snapshot: {
          cpu_usage_percent: 63.7,
          load: { one: 1.24 },
          memory: { total_bytes: 200, used_bytes: 100 },
        },
      }),
    ]);

    const row = wrapper.find('[data-test="health-matrix-row"]');
    const latency = row.find('[data-test="health-matrix-latency"]');
    const load = row.find('[data-test="health-matrix-load"]');
    const cpu = row.find('[data-test="health-matrix-cpu"]');
    const memory = row.find('[data-test="health-matrix-memory"]');

    expect(latency.text()).toBe('42');
    expect(latency.classes()).toContain('green');
    expect(load.text()).toBe('1.24');
    expect(load.classes()).toContain('yellow');
    expect(cpu.text()).toBe('64%');
    expect(cpu.classes()).toContain('yellow');
    expect(memory.text()).toBe('50%');
    expect(memory.classes()).toContain('yellow');
  });

  it('uses muted placeholders when live metrics are unavailable', async () => {
    const wrapper = await mountMatrix([
      makeNode({
        identity: { node_id: 'alpha', node_label: 'Alpha', hostname: 'alpha', tags: [] },
        latency_ms: null,
        snapshot: null,
      }),
    ]);

    const row = wrapper.find('[data-test="health-matrix-row"]');
    for (const selector of [
      '[data-test="health-matrix-latency"]',
      '[data-test="health-matrix-load"]',
      '[data-test="health-matrix-cpu"]',
      '[data-test="health-matrix-memory"]',
    ]) {
      const cell = row.find(selector);
      expect(cell.text()).toBe('—');
      expect(cell.classes()).toContain('muted');
    }
  });
});
