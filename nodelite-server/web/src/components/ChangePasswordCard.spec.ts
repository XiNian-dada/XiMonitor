import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { apiClient } from '@/api';
import { AUTH_TIMESTAMP_KEY } from '@/auth/expiry';
import ChangePasswordCard from './ChangePasswordCard.vue';

vi.mock('@/api', async () => {
  const actual = await vi.importActual<typeof import('@/api')>('@/api');
  return { ...actual, apiClient: { ...actual.apiClient, changePassword: vi.fn() } };
});

const mockChange = vi.mocked(apiClient.changePassword);

const FAKE_DICT = {
  en: {
    'settings.password.title': 'Change Password',
    'settings.password.current': 'Current password',
    'settings.password.new': 'New password',
    'settings.password.generate': 'Generate',
    'settings.password.submit': 'Update password',
    'settings.password.saving': 'Updating…',
    'settings.password.saved': 'Password updated. Please sign in again.',
    'settings.password.failed': 'Failed: {error}',
  },
  'zh-CN': {},
};

const Stub = defineComponent({ render: () => h('div') });

function mountCard() {
  return mount(ChangePasswordCard, { global: { plugins: [getI18n()] } });
}

describe('ChangePasswordCard', () => {
  let assignSpy: ReturnType<typeof vi.fn>;
  let originalLocation: Location;

  beforeEach(async () => {
    __resetI18nForTest();
    mockChange.mockReset();
    window.localStorage.clear();
    originalLocation = window.location;
    assignSpy = vi.fn();
    Object.defineProperty(window, 'location', {
      configurable: true,
      value: { ...originalLocation, assign: assignSpy },
    });
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
    vi.useRealTimers();
    Object.defineProperty(window, 'location', { configurable: true, value: originalLocation });
  });

  it('generate fills a strong new password and reveals it', async () => {
    const wrapper = mountCard();
    const field = wrapper.find('[data-test="password-new"]');
    expect(field.attributes('type')).toBe('password');
    await wrapper.find('[data-test="password-generate"]').trigger('click');
    const input = field.element as HTMLInputElement;
    expect(input.value.length).toBeGreaterThanOrEqual(8);
    // revealed so the user can read/copy before the post-submit redirect
    expect(field.attributes('type')).toBe('text');
  });

  it('on success: clears the auth timestamp and navigates to logout-and-reauth', async () => {
    vi.useFakeTimers();
    window.localStorage.setItem(AUTH_TIMESTAMP_KEY, '12345');
    mockChange.mockResolvedValueOnce({ ok: true, message: '' });
    const wrapper = mountCard();
    await wrapper.find('[data-test="password-current"]').setValue('old');
    await wrapper.find('[data-test="password-new"]').setValue('newpassword1');
    await wrapper.find('[data-test="password-form"]').trigger('submit');
    await flushPromises();

    expect(mockChange).toHaveBeenCalledWith({ current_password: 'old', new_password: 'newpassword1' });
    expect(wrapper.find('[data-test="settings-message"]').text()).toContain('sign in again');
    // submit stays disabled through the logout window (no stale double-submit)
    expect(wrapper.find('[data-test="password-submit"]').attributes('disabled')).toBeDefined();
    // navigation happens after the brief delay
    expect(assignSpy).not.toHaveBeenCalled();
    vi.advanceTimersByTime(900);
    expect(window.localStorage.getItem(AUTH_TIMESTAMP_KEY)).toBeNull();
    expect(assignSpy).toHaveBeenCalledWith('/logout-and-reauth');
  });

  it('surfaces the server error and does not navigate', async () => {
    const { ApiError } = await import('@/api/client');
    mockChange.mockRejectedValueOnce(new ApiError(400, JSON.stringify({ ok: false, message: 'wrong password' })));
    const wrapper = mountCard();
    await wrapper.find('[data-test="password-current"]').setValue('bad');
    await wrapper.find('[data-test="password-new"]').setValue('newpassword1');
    await wrapper.find('[data-test="password-form"]').trigger('submit');
    await flushPromises();
    expect(wrapper.find('[data-test="settings-message"]').text()).toContain('wrong password');
    expect(assignSpy).not.toHaveBeenCalled();
  });
});
