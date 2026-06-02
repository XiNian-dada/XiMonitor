import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import type { ChartPoint } from '@/lib/chart/chartData';
import MetricChart from './MetricChart.vue';
import ChartModal from './ChartModal.vue';

const FAKE_DICT = { en: { 'node.waiting_history': 'Waiting…' }, 'zh-CN': {} };

const Stub = defineComponent({ render: () => h('div') });

function pts(values: number[]): ChartPoint[] {
  return values.map((value, i) => ({ ts: i * 60_000, value }));
}

describe('ChartModal', () => {
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

  it('renders the chart + title and emits close (parent gates visibility)', async () => {
    const wrapper = mount(ChartModal, {
      props: { title: 'CPU', points: pts([10, 90]), valueKind: 'percent', color: 'var(--chart-cpu)' },
      global: { plugins: [getI18n()] },
    });
    expect(wrapper.find('[data-test="chart-modal"]').exists()).toBe(true);
    expect(wrapper.find('.chart-modal__title').text()).toBe('CPU');
    expect(wrapper.find('[data-test="metric-chart"]').exists()).toBe(true);

    await wrapper.find('[data-test="chart-modal-close"]').trigger('click');
    expect(wrapper.emitted('close')).toHaveLength(1);
  });

  it('passes the selected spike clipping state into the chart', () => {
    const wrapper = mount(ChartModal, {
      props: {
        title: 'Network',
        points: pts([10, 1000]),
        valueKind: 'rate',
        color: 'var(--chart-network-down)',
        clipSpikes: false,
      },
      global: { plugins: [getI18n()] },
    });

    expect(wrapper.findComponent(MetricChart).props('clipSpikes')).toBe(false);
  });
});
