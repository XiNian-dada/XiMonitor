import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h, reactive } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { viewToDraft, type SmtpDraft } from '@/lib/alertsDraft';
import { makeAlertSettingsView } from '@/api/__fixtures__/nodes';
import SmtpChannelCard from './SmtpChannelCard.vue';

const FAKE_DICT = {
  en: {
    'alerts.secret.keep': 'leave blank to keep',
    'settings.disabled': 'Disabled',
  },
  'zh-CN': {},
};
const Stub = defineComponent({ render: () => h('div') });

function mountCard(smtp: SmtpDraft) {
  return mount(SmtpChannelCard, {
    props: { modelValue: smtp, 'onUpdate:modelValue': () => {} },
    global: { plugins: [getI18n()] },
  });
}

describe('SmtpChannelCard', () => {
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

  it('renders draft values and the keep-secret placeholder when configured', () => {
    const smtp = reactive(viewToDraft(makeAlertSettingsView()).smtp);
    const wrapper = mountCard(smtp);
    expect((wrapper.find('[data-test="smtp-host"]').element as HTMLInputElement).value).toBe('smtp.example.com');
    expect(wrapper.find('[data-test="smtp-password"]').attributes('placeholder')).toBe('leave blank to keep');
  });

  it('binds enabled + password edits back into the draft slice', async () => {
    const smtp = reactive(viewToDraft(makeAlertSettingsView({ smtp: { enabled: false } })).smtp);
    const wrapper = mountCard(smtp);
    expect(wrapper.find('[data-test="smtp-collapsed"]').exists()).toBe(true);
    expect(wrapper.find('[data-test="smtp-form"]').exists()).toBe(false);

    await wrapper.find('[data-test="smtp-enabled"]').setValue(true);
    expect(smtp.enabled).toBe(true);
    expect(wrapper.find('[data-test="smtp-form"]').exists()).toBe(true);
    await wrapper.find('[data-test="smtp-password"]').setValue('new-secret');
    expect(smtp.password).toBe('new-secret');
  });

  it('clear-password disables the password input and sets the flag', async () => {
    const smtp = reactive(viewToDraft(makeAlertSettingsView()).smtp);
    const wrapper = mountCard(smtp);
    await wrapper.find('[data-test="smtp-clear-password"]').setValue(true);
    expect(smtp.clear_password).toBe(true);
    expect(wrapper.find('[data-test="smtp-password"]').attributes('disabled')).toBeDefined();
  });

  it('edits recipients through CsvField', async () => {
    const smtp = reactive(viewToDraft(makeAlertSettingsView()).smtp);
    const wrapper = mountCard(smtp);
    await wrapper.find('[data-test="smtp-recipients"]').setValue('a@x.com, b@x.com');
    expect(smtp.recipients).toEqual(['a@x.com', 'b@x.com']);
  });
});
