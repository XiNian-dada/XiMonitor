import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import type { ChartPoint } from '@/lib/chart/chartData';
import MetricChart from './MetricChart.vue';

const FAKE_DICT = {
  en: { 'node.waiting_history': 'Waiting for enough history samples…' },
  'zh-CN': { 'node.waiting_history': '等待足够的历史样本…' },
};

const Stub = defineComponent({ render: () => h('div') });

function pts(values: Array<number | null>): ChartPoint[] {
  return values.map((value, i) => ({ ts: i * 60_000, value }));
}

function mountChart(props: Record<string, unknown>) {
  return mount(MetricChart, { props, global: { plugins: [getI18n()] } });
}

describe('MetricChart', () => {
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

  it('renders the empty placeholder when there are no numeric points', () => {
    const wrapper = mountChart({ points: pts([null, null]), valueKind: 'percent' });
    expect(wrapper.find('[data-test="metric-chart-empty"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="metric-chart-svg"]').exists()).toBe(false);
  });

  it('renders an area chart: svg with a line + area path + grid labels', () => {
    const wrapper = mountChart({
      points: pts([10, 50, 90]),
      valueKind: 'percent',
      color: 'var(--chart-cpu)',
      label: 'CPU',
    });
    expect(wrapper.find('[data-test="metric-chart-svg"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="metric-chart-line"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="metric-chart-area"]').exists()).toBe(true);
    expect(wrapper.findAll('text').length).toBeGreaterThan(0);
  });

  it('renders x-axis time ticks from point timestamps', () => {
    const wrapper = mountChart({
      points: pts([10, 50, 90]),
      valueKind: 'percent',
      color: 'var(--chart-cpu)',
      label: 'CPU',
    });
    expect(wrapper.findAll('[data-test="metric-chart-x-tick"]').length).toBeGreaterThanOrEqual(2);
  });

  it('clips spikes by default to match the legacy chart state', () => {
    const wrapper = mountChart({
      points: pts([...Array.from({ length: 50 }, (_, i) => i + 1), 1000]),
      valueKind: 'percent',
      color: 'var(--chart-cpu)',
      label: 'CPU',
    });
    const labels = wrapper.findAll('.metric-chart__grid text').map((text) => text.text());
    expect(labels.at(-1)).not.toBe('1000%');
  });

  it('renders a multi-series chart with a line per series and no area', () => {
    const wrapper = mountChart({
      series: [
        { label: 'down', color: 'var(--chart-network-down)', points: pts([100, 200, 300]) },
        { label: 'up', color: 'var(--chart-network-up)', points: pts([10, 20, 30]) },
      ],
      valueKind: 'rate',
    });
    expect(wrapper.findAll('[data-test="metric-chart-line"]')).toHaveLength(2);
    expect(wrapper.find('[data-test="metric-chart-area"]').exists()).toBe(false);
  });
});
