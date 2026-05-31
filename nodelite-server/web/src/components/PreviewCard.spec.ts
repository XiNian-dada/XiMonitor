import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { makeAlertPreview } from '@/api/__fixtures__/nodes';
import type { AlertPreview } from '@/api';
import PreviewCard from './PreviewCard.vue';

const FAKE_DICT = {
  en: {
    'alerts.preview.title': 'Preview',
    'alerts.preview.empty': 'Save once to preview.',
    'alerts.preview.total_nodes': 'Total nodes',
    'alerts.preview.offline_nodes': 'Offline nodes',
    'alerts.preview.latency_nodes': 'High latency nodes',
    'alerts.preview.cpu_hot_nodes': 'High CPU nodes',
    'alerts.preview.memory_hot_nodes': 'High memory nodes',
    'alerts.preview.triggered_rules': 'Triggered rules',
    'alerts.preview.no_triggered_rules': 'No rules would currently fire.',
    'alerts.preview.highlights': 'Inspection highlights',
    'alerts.preview.no_highlights': 'No daily inspection highlights yet.',
    'alerts.severity.warning': 'Warning',
    'alerts.severity.critical': 'Critical',
  },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function mountCard(preview: AlertPreview | null) {
  return mount(PreviewCard, { props: { preview }, global: { plugins: [getI18n()] } });
}

describe('PreviewCard', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(FAKE_DICT) } as unknown as Response),
    );
    await setupI18n(createApp(Stub));
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
  });

  it('shows the empty state when there is no preview', () => {
    const wrapper = mountCard(null);
    expect(wrapper.find('[data-test="preview-empty"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="preview-summary"]').exists()).toBe(false);
  });

  it('renders the inspection summary counts', () => {
    const wrapper = mountCard(
      makeAlertPreview({
        inspection: {
          total_nodes: 9,
          offline_nodes: 2,
          latency_nodes: 1,
          cpu_hot_nodes: 3,
          memory_hot_nodes: 0,
          highlights: [],
        },
      }),
    );
    const values = wrapper.findAll('[data-test="preview-summary"] .summary-value').map((n) => n.text());
    expect(values).toEqual(['9', '2', '1', '3', '0']);
  });

  it('renders triggered rules with a severity badge and the empty highlight state', () => {
    const wrapper = mountCard(
      makeAlertPreview({
        triggered_rules: [
          { rule_id: 'r1', rule_name: 'CPU hot', severity: 'critical', node_ids: ['node-a', 'node-b'] },
        ],
      }),
    );
    const item = wrapper.find('[data-test="preview-triggered"] .preview-item');
    expect(item.text()).toContain('Critical');
    expect(item.text()).toContain('CPU hot');
    expect(item.text()).toContain('node-a, node-b');
    expect(wrapper.find('[data-test="preview-no-triggered"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="preview-no-highlights"]').exists()).toBe(true);
  });

  it('renders highlights using node_label, falling back to node_id', () => {
    const wrapper = mountCard(
      makeAlertPreview({
        inspection: {
          total_nodes: 1,
          offline_nodes: 0,
          latency_nodes: 0,
          cpu_hot_nodes: 0,
          memory_hot_nodes: 0,
          highlights: [
            { node_id: 'node-a', node_label: 'Node A', reasons: ['offline', 'high cpu'] },
            { node_id: 'node-b', node_label: '', reasons: ['high latency'] },
          ],
        },
      }),
    );
    const items = wrapper.findAll('[data-test="preview-highlights"] .preview-item');
    expect(items).toHaveLength(2);
    expect(items[0]?.text()).toContain('Node A');
    expect(items[0]?.text()).toContain('offline, high cpu');
    expect(items[1]?.text()).toContain('node-b');
  });
});
