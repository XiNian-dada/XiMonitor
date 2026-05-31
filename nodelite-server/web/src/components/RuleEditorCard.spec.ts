import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h, reactive } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { blankRule, type RuleDraft } from '@/lib/alertsDraft';
import RuleEditorCard from './RuleEditorCard.vue';

const FAKE_DICT = {
  en: {
    'alerts.rules.name': 'Rule name',
    'alerts.rules.enabled': 'Enabled',
    'alerts.rules.remove': 'Remove',
    'alerts.rules.details': 'Details',
    'alerts.rules.id': 'ID',
    'alerts.rules.metric': 'Metric',
    'alerts.rules.comparator': 'Comparator',
    'alerts.rules.threshold': 'Threshold',
    'alerts.rules.window_minutes': 'Window (min)',
    'alerts.rules.cooldown_minutes': 'Cooldown (min)',
    'alerts.rules.severity': 'Severity',
    'alerts.rules.scope_mode': 'Scope',
    'alerts.rules.node_ids': 'Node IDs',
    'alerts.rules.tags': 'Tags',
    'alerts.rules.send_resolved': 'Send resolved',
    'alerts.inspection.delivery': 'Delivery',
    'alerts.metric.cpu': 'CPU',
    'alerts.metric.memory': 'Memory',
    'alerts.metric.disk': 'Disk',
    'alerts.metric.latency': 'Latency',
    'alerts.metric.offline': 'Offline',
    'alerts.comparator.gt': '>',
    'alerts.comparator.lt': '<',
    'alerts.severity.warning': 'Warning',
    'alerts.severity.critical': 'Critical',
    'alerts.scope.all': 'All nodes',
    'alerts.scope.node_ids': 'Node IDs',
    'alerts.scope.tags': 'Tags',
    'alerts.channel.smtp': 'Email',
    'alerts.channel.webhook': 'Webhook',
    'common.not_available': 'N/A',
  },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function mountCard(rule: RuleDraft) {
  return mount(RuleEditorCard, {
    props: { modelValue: rule, 'onUpdate:modelValue': () => {} },
    global: { plugins: [getI18n()] },
  });
}

describe('RuleEditorCard', () => {
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

  it('renders the title and a human-readable expression', () => {
    const rule = reactive({ ...blankRule(), name: 'CPU hot' });
    const wrapper = mountCard(rule);
    expect(wrapper.find('[data-test="rule-title"]').text()).toBe('CPU hot');
    // metric > threshold · window m · scope · delivery
    expect(wrapper.find('[data-test="rule-expression"]').text()).toBe('CPU > 85 · 5m · All nodes · Email');
  });

  it('falls back to id then label when name is blank', () => {
    const rule = reactive({ ...blankRule(), name: '', id: 'rule-7' });
    const wrapper = mountCard(rule);
    expect(wrapper.find('[data-test="rule-title"]').text()).toBe('rule-7');
  });

  it('binds field edits back into the bound rule, coercing numbers', async () => {
    const rule = reactive(blankRule());
    const wrapper = mountCard(rule);
    await wrapper.find('[data-test="rule-name"]').setValue('Memory hot');
    expect(rule.name).toBe('Memory hot');
    await wrapper.find('[data-test="rule-threshold"]').setValue('90');
    expect(rule.threshold).toBe(90);
    expect(typeof rule.threshold).toBe('number');
    await wrapper.find('[data-test="rule-metric"]').setValue('memory_usage_percent');
    expect(rule.metric).toBe('memory_usage_percent');
  });

  it('gates the scope target field on scope_mode', async () => {
    const rule = reactive(blankRule());
    const wrapper = mountCard(rule);
    expect(wrapper.find('[data-test="rule-node-ids"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="rule-tags"]').exists()).toBe(false);

    rule.scope_mode = 'node_ids';
    await wrapper.vm.$nextTick();
    expect(wrapper.find('[data-test="rule-node-ids"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="rule-tags"]').exists()).toBe(false);

    rule.scope_mode = 'tags';
    await wrapper.vm.$nextTick();
    expect(wrapper.find('[data-test="rule-node-ids"]').exists()).toBe(false);
    expect(wrapper.find('[data-test="rule-tags"]').exists()).toBe(true);
  });

  it('edits node_ids through CsvField when scoped to node_ids', async () => {
    const rule = reactive({ ...blankRule(), scope_mode: 'node_ids' as const });
    const wrapper = mountCard(rule);
    await wrapper.find('[data-test="rule-node-ids"]').setValue('node-a, node-b');
    expect(rule.node_ids).toEqual(['node-a', 'node-b']);
  });

  it('toggles delivery channels', async () => {
    const rule = reactive(blankRule());
    const wrapper = mountCard(rule);
    expect(rule.delivery).toEqual(['smtp']);
    await wrapper.find('[data-test="delivery-webhook"]').setValue(true);
    expect(rule.delivery).toEqual(['smtp', 'webhook']);
  });

  it('emits remove when the remove button is clicked', async () => {
    const rule = reactive(blankRule());
    const wrapper = mountCard(rule);
    await wrapper.find('[data-test="rule-remove"]').trigger('click');
    expect(wrapper.emitted('remove')).toHaveLength(1);
  });
});
