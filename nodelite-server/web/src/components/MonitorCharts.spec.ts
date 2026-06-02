import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { makeNodeStatus } from '@/api/__fixtures__/nodes';
import type { HistoryPoint } from '@/api';
import MonitorCharts from './MonitorCharts.vue';

const FAKE_DICT = {
  en: {
    'node.cpu_usage': 'CPU Usage',
    'node.memory_usage': 'Memory Usage',
    'node.network_traffic': 'Network Traffic',
    'node.latency_history': 'RTT',
    'node.chart.zoom': 'Open enlarged chart',
    'node.preset.last_3h': '3h',
    'node.preset.last_24h': '24h',
    'node.preset.last_3d': '3d',
    'node.preset.last_7d': '7d',
    'node.preset.last_14d': '14d',
    'index.node.download': 'Down',
    'index.node.upload': 'Up',
    'node.waiting_history': 'Waiting…',
  },
  'zh-CN': { 'node.cpu_usage': 'CPU 使用率' },
};

const Stub = defineComponent({ render: () => h('div') });

function hp(recorded_at: string): HistoryPoint {
  return {
    node_id: 'n',
    recorded_at,
    cpu_usage_percent: 10,
    memory_used_percent: 20,
    rx_bytes_per_sec: 100,
    tx_bytes_per_sec: 50,
    latency_ms: 5,
    disk_used_percent: null,
  };
}

function mountMonitor() {
  return mount(MonitorCharts, {
    props: {
      node: makeNodeStatus(),
      history: [hp('2026-05-29T00:00:00Z'), hp('2026-05-29T00:05:00Z')],
      activeKey: 'last_24h' as const,
    },
    global: { plugins: [getI18n()] },
  });
}

describe('MonitorCharts', () => {
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

  it('renders five preset buttons with the active one marked', () => {
    const wrapper = mountMonitor();
    expect(wrapper.findAll('.preset-button')).toHaveLength(5);
    expect(wrapper.find('[data-test="preset-last_24h"]').classes()).toContain('active');
  });

  it('renders four big charts', () => {
    const wrapper = mountMonitor();
    expect(wrapper.findAll('[data-test="metric-chart"]')).toHaveLength(4);
  });

  it('keeps memory charts on a full 100 percent scale', () => {
    const wrapper = mountMonitor();
    const charts = wrapper.findAll('[data-test="metric-chart"]');
    expect(charts[1]?.text()).toContain('100%');
  });

  it('emits selectPreset when a preset is clicked', async () => {
    const wrapper = mountMonitor();
    await wrapper.find('[data-test="preset-last_7d"]').trigger('click');
    expect(wrapper.emitted('selectPreset')?.[0]).toEqual(['last_7d']);
  });

  it('emits zoom with the metric when a zoom button is clicked', async () => {
    const wrapper = mountMonitor();
    await wrapper.find('[data-test="zoom-network"]').trigger('click');
    expect(wrapper.emitted('zoom')?.[0]).toEqual(['network']);
  });
});
