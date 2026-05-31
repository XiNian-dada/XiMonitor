import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h, reactive } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { viewToDraft, type RuleDraft } from '@/lib/alertsDraft';
import { makeAlertSettingsView } from '@/api/__fixtures__/nodes';
import RuleList from './RuleList.vue';

const FAKE_DICT = {
  en: {
    'alerts.rules.title': 'Alert Rules',
    'alerts.rules.note': 'note',
    'alerts.rules.add': 'Add rule',
    'alerts.rules.empty': 'No alert rules yet.',
    // keys the nested RuleEditorCard renders
    'alerts.rules.name': 'Rule name',
    'alerts.rules.enabled': 'Enabled',
    'alerts.rules.remove': 'Remove',
    'alerts.rules.details': 'Details',
    'alerts.rules.id': 'ID',
    'alerts.rules.metric': 'Metric',
    'alerts.rules.comparator': 'Comparator',
    'alerts.rules.threshold': 'Threshold',
    'alerts.rules.window_minutes': 'Window',
    'alerts.rules.cooldown_minutes': 'Cooldown',
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

function mountList(rules: RuleDraft[]) {
  return mount(RuleList, {
    props: { modelValue: rules, 'onUpdate:modelValue': () => {} },
    global: { plugins: [getI18n()] },
  });
}

describe('RuleList', () => {
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

  it('renders one editor card per rule', () => {
    const rules = reactive(viewToDraft(makeAlertSettingsView()).rules);
    const wrapper = mountList(rules);
    expect(wrapper.findAll('[data-test="rule-card"]')).toHaveLength(1);
    expect(wrapper.find('[data-test="rule-list-empty"]').exists()).toBe(false);
  });

  it('shows the empty state when there are no rules', () => {
    const rules = reactive<RuleDraft[]>([]);
    const wrapper = mountList(rules);
    expect(wrapper.find('[data-test="rule-list-empty"]').exists()).toBe(true);
    expect(wrapper.findAll('[data-test="rule-card"]')).toHaveLength(0);
  });

  it('appends a blank rule on add', async () => {
    const rules = reactive<RuleDraft[]>([]);
    const wrapper = mountList(rules);
    await wrapper.find('[data-test="rule-add"]').trigger('click');
    expect(rules).toHaveLength(1);
    expect(wrapper.findAll('[data-test="rule-card"]')).toHaveLength(1);
  });

  it('removes the rule whose card emitted remove', async () => {
    const rules = reactive(viewToDraft(makeAlertSettingsView({ rules: makeAlertSettingsView().rules })).rules);
    // Seed a second rule with a distinct uid by adding through the UI.
    const wrapper = mountList(rules);
    await wrapper.find('[data-test="rule-add"]').trigger('click');
    expect(rules).toHaveLength(2);
    const firstUid = rules[0]?.uid;
    await wrapper.findAll('[data-test="rule-remove"]')[0]?.trigger('click');
    expect(rules).toHaveLength(1);
    expect(rules.some((r) => r.uid === firstUid)).toBe(false);
  });
});
