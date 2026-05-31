import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import ReauthFields from './ReauthFields.vue';

const FAKE_DICT = {
  en: { 'settings.password.current': 'Current password', 'settings.security.verification_code': '6-digit code' },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function mountFields(props: {
  twoFactorEnabled: boolean;
  variant?: 'server-update' | 'standard' | 'both';
}) {
  return mount(ReauthFields, { props, global: { plugins: [getI18n()] } });
}

describe('ReauthFields', () => {
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

  it('server-update variant: code only when 2FA on, password only when off', () => {
    const on = mountFields({ twoFactorEnabled: true, variant: 'server-update' });
    expect(on.find('[data-test="reauth-code"]').exists()).toBe(true);
    expect(on.find('[data-test="reauth-password"]').exists()).toBe(false);

    const off = mountFields({ twoFactorEnabled: false, variant: 'server-update' });
    expect(off.find('[data-test="reauth-password"]').exists()).toBe(true);
    expect(off.find('[data-test="reauth-code"]').exists()).toBe(false);
  });

  it('standard variant: password always, code only when 2FA on', () => {
    const off = mountFields({ twoFactorEnabled: false });
    expect(off.find('[data-test="reauth-password"]').exists()).toBe(true);
    expect(off.find('[data-test="reauth-code"]').exists()).toBe(false);

    const on = mountFields({ twoFactorEnabled: true });
    expect(on.find('[data-test="reauth-password"]').exists()).toBe(true);
    expect(on.find('[data-test="reauth-code"]').exists()).toBe(true);
  });

  it('both variant: password + code always', () => {
    const w = mountFields({ twoFactorEnabled: false, variant: 'both' });
    expect(w.find('[data-test="reauth-password"]').exists()).toBe(true);
    expect(w.find('[data-test="reauth-code"]').exists()).toBe(true);
  });

  it('updates v-models on input', async () => {
    const w = mountFields({ twoFactorEnabled: true, variant: 'both' });
    await w.find('[data-test="reauth-password"]').setValue('pw');
    await w.find('[data-test="reauth-code"]').setValue('123456');
    expect(w.emitted('update:currentPassword')?.at(-1)).toEqual(['pw']);
    expect(w.emitted('update:code')?.at(-1)).toEqual(['123456']);
  });
});
