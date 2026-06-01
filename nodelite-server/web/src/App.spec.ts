import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { mount } from '@vue/test-utils';
import { defineComponent, h } from 'vue';
import { createMemoryHistory, createRouter } from 'vue-router';

import App from './App.vue';
import { useWebSocket } from '@/ws';

const Placeholder = defineComponent({ render: () => h('div', { 'data-test': 'route-stub' }) });

const router = createRouter({
  history: createMemoryHistory(),
  routes: [{ path: '/', name: 'dashboard', component: Placeholder }],
});

describe('App.vue', () => {
  let fetchSpy: any; // eslint-disable-line @typescript-eslint/no-explicit-any

  beforeEach(() => {
    // Mock fetch to avoid auth probe errors in tests
    fetchSpy = vi.spyOn(global, 'fetch').mockResolvedValue({
      status: 200,
      ok: true,
    } as Response);
  });

  afterEach(() => {
    fetchSpy.mockRestore();
  });

  it('renders the active route via RouterView', async () => {
    await router.push('/');
    await router.isReady();

    const wrapper = mount(App, { global: { plugins: [router] } });
    expect(wrapper.find('[data-test="route-stub"]').exists()).toBe(true);
  });

  it('connects WebSocket on mount and destroys on unmount', async () => {
    await router.push('/');
    await router.isReady();

    const ws = useWebSocket();
    const connectSpy = vi.spyOn(ws, 'connect');
    const destroySpy = vi.spyOn(ws, 'destroy');

    const wrapper = mount(App, { global: { plugins: [router] } });

    expect(connectSpy).toHaveBeenCalledTimes(1);

    wrapper.unmount();

    expect(destroySpy).toHaveBeenCalledTimes(1);

    connectSpy.mockRestore();
    destroySpy.mockRestore();
  });
});
