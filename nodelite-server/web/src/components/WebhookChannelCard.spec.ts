import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h, reactive } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { viewToDraft, type WebhookDraft } from '@/lib/alertsDraft';
import { makeAlertSettingsView } from '@/api/__fixtures__/nodes';
import WebhookChannelCard from './WebhookChannelCard.vue';

const FAKE_DICT = {
  en: {
    'alerts.secret.keep': 'leave blank to keep',
    'settings.disabled': 'Disabled',
  },
  'zh-CN': {},
};
const Stub = defineComponent({ render: () => h('div') });

function mountCard(webhook: WebhookDraft) {
  return mount(WebhookChannelCard, {
    props: { modelValue: webhook, 'onUpdate:modelValue': () => {} },
    global: { plugins: [getI18n()] },
  });
}

describe('WebhookChannelCard', () => {
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

  it('shows the keep-secret placeholder only when a secret is configured', () => {
    const configured = reactive(
      viewToDraft(makeAlertSettingsView({ webhook: { enabled: true, secret_configured: true } })).webhook,
    );
    expect(mountCard(configured).find('[data-test="webhook-secret"]').attributes('placeholder')).toBe(
      'leave blank to keep',
    );
    const blank = reactive(
      viewToDraft(makeAlertSettingsView({ webhook: { enabled: true, secret_configured: false } })).webhook,
    );
    expect(mountCard(blank).find('[data-test="webhook-secret"]').attributes('placeholder')).toBe('');
  });

  it('binds url + secret edits and the clear flag', async () => {
    const webhook = reactive(viewToDraft(makeAlertSettingsView({ webhook: { enabled: true } })).webhook);
    const wrapper = mountCard(webhook);
    await wrapper.find('[data-test="webhook-url"]').setValue('https://hooks.example.com/y');
    expect(webhook.url).toBe('https://hooks.example.com/y');
    await wrapper.find('[data-test="webhook-clear-secret"]').setValue(true);
    expect(webhook.clear_secret).toBe(true);
    expect(wrapper.find('[data-test="webhook-secret"]').attributes('disabled')).toBeDefined();
  });

  it('collapses details while disabled and expands after enabling', async () => {
    const webhook = reactive(viewToDraft(makeAlertSettingsView({ webhook: { enabled: false } })).webhook);
    const wrapper = mountCard(webhook);

    expect(wrapper.find('[data-test="webhook-collapsed"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="webhook-form"]').exists()).toBe(false);

    await wrapper.find('[data-test="webhook-enabled"]').setValue(true);
    expect(wrapper.find('[data-test="webhook-form"]').exists()).toBe(true);
  });
});
