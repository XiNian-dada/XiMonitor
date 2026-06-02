import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { makeNodeStatus } from '@/api/__fixtures__/nodes';
import type { HistoryPoint } from '@/api';
import OverviewCharts from './OverviewCharts.vue';

const FAKE_DICT = {
  en: {
    'node.cpu_usage': 'CPU Usage',
    'node.memory_usage': 'Memory Usage',
    'node.network_traffic': 'Network Traffic',
    'node.latency_history': 'RTT',
    'node.chart.average': 'Avg {value}',
    'index.node.download': 'Down',
    'index.node.upload': 'Up',
    'node.waiting_history': 'Waiting…',
  },
  'zh-CN': { 'node.cpu_usage': 'CPU 使用率' },
};

const Stub = defineComponent({ render: () => h('div') });

function hp(recorded_at: string, over: Partial<HistoryPoint> = {}): HistoryPoint {
  return {
    node_id: 'n',
    recorded_at,
    cpu_usage_percent: 10,
    memory_used_percent: 20,
    rx_bytes_per_sec: 100,
    tx_bytes_per_sec: 50,
    latency_ms: 5,
    disk_used_percent: null,
    ...over,
  };
}

describe('OverviewCharts', () => {
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

  it('renders four chart cards, each with a MetricChart', () => {
    const wrapper = mount(OverviewCharts, {
      props: {
        node: makeNodeStatus({ snapshot: { ...makeNodeStatus().snapshot!, cpu_usage_percent: 42 } }),
        history: [hp('2026-05-29T00:00:00Z'), hp('2026-05-29T00:01:00Z', { cpu_usage_percent: 60 })],
      },
      global: { plugins: [getI18n()] },
    });
    expect(wrapper.findAll('.chart-card')).toHaveLength(4);
    expect(wrapper.findAll('[data-test="metric-chart"]')).toHaveLength(4);
  });

  it('shows the current cpu/memory/latency values from the snapshot', () => {
    const wrapper = mount(OverviewCharts, {
      props: {
        node: makeNodeStatus({
          snapshot: {
            ...makeNodeStatus().snapshot!,
            cpu_usage_percent: 42,
            memory: { total_bytes: 100, used_bytes: 25, available_bytes: 75, swap_total_bytes: 0, swap_used_bytes: 0 },
          },
          latency_ms: 7,
        }),
        history: [hp('2026-05-29T00:00:00Z')],
      },
      global: { plugins: [getI18n()] },
    });
    expect(wrapper.find('[data-test="now-cpu"]').text()).toBe('42%');
    expect(wrapper.find('[data-test="now-memory"]').text()).toBe('25%');
    expect(wrapper.find('[data-test="now-rtt"]').text()).toBe('7.0 ms');
  });

  it('keeps memory charts on a full 100 percent scale', () => {
    const wrapper = mount(OverviewCharts, {
      props: {
        node: makeNodeStatus(),
        history: [
          hp('2026-05-29T00:00:00Z', { memory_used_percent: 74 }),
          hp('2026-05-29T00:01:00Z', { memory_used_percent: 76 }),
        ],
      },
      global: { plugins: [getI18n()] },
    });
    const charts = wrapper.findAll('[data-test="metric-chart"]');
    expect(charts[1]?.text()).toContain('100%');
  });
});
