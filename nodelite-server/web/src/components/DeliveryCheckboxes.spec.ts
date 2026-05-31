import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import type { AlertChannel } from '@/api';
import DeliveryCheckboxes from './DeliveryCheckboxes.vue';

const FAKE_DICT = {
  en: { 'alerts.channel.smtp': 'Email', 'alerts.channel.webhook': 'Webhook' },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function mountBoxes(modelValue: AlertChannel[]) {
  return mount(DeliveryCheckboxes, {
    props: { modelValue, 'onUpdate:modelValue': () => {} },
    global: { plugins: [getI18n()] },
  });
}

describe('DeliveryCheckboxes', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(FAKE_DICT) } as unknown as Response),
    );
    const dummy = createApp(Stub);
    await setupI18n(dummy);
  });

  afterEach(() => {
    __resetI18nForTest();
    vi.unstubAllGlobals();
  });

  it('reflects the selected channels as checked', () => {
    const wrapper = mountBoxes(['webhook']);
    expect((wrapper.find('[data-test="delivery-smtp"]').element as HTMLInputElement).checked).toBe(false);
    expect((wrapper.find('[data-test="delivery-webhook"]').element as HTMLInputElement).checked).toBe(true);
  });

  it('adds a channel in canonical order when checked', async () => {
    const wrapper = mountBoxes(['webhook']);
    await wrapper.find('[data-test="delivery-smtp"]').setValue(true);
    // smtp added but order stays smtp, webhook regardless of click order
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['smtp', 'webhook']);
  });

  it('removes a channel when unchecked', async () => {
    const wrapper = mountBoxes(['smtp', 'webhook']);
    await wrapper.find('[data-test="delivery-smtp"]').setValue(false);
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['webhook']);
  });
});
