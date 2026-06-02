import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { createApp, defineComponent, h } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { nextTick } from 'vue';
import { setupI18n, getI18n, __resetI18nForTest } from '@/i18n';
import { __resetWorldGeoJsonForTest } from '@/composables/useWorldGeoJson';
import { useNodesStore } from '@/stores/nodes';
import { useTheme } from '@/composables/useTheme';
import { makeNode } from '@/api/__fixtures__/nodes';
import NodeMap from './NodeMap.vue';

const FAKE_DICT = {
  en: {
    'index.map.title': 'Global Distribution',
    'index.map.legend_online': 'Online',
    'index.map.legend_latency': 'High latency',
    'index.map.legend_offline': 'Offline',
  },
  'zh-CN': {
    'index.map.title': '全球分布',
    'index.map.legend_online': '在线',
    'index.map.legend_latency': '高延迟',
    'index.map.legend_offline': '离线',
  },
};

const Stub = defineComponent({ render: () => h('div') });

async function mountWithNodes(nodes: ReturnType<typeof makeNode>[]) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const store = useNodesStore();
  store.applyServerState(nodes, new Date().toISOString());
  const wrapper = mount(NodeMap, { global: { plugins: [pinia, getI18n()] } });
  await flushPromises();
  return wrapper;
}

describe('NodeMap', () => {
  beforeEach(async () => {
    __resetI18nForTest();
    __resetWorldGeoJsonForTest();
    // jsdom has no canvas 2D context (it throws "Not implemented"); return
    // null so paintWorldDotMap no-ops quietly. The painting itself is
    // covered by lib/map/landMask.spec.ts.
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    // i18n dictionary fetch succeeds; the world GeoJSON fetch fails so the
    // component keeps the fallback mask (paint no-ops in jsdom regardless,
    // since canvas has no 2D context).
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        if (String(url).includes('ui-i18n.json')) {
          return Promise.resolve({
            ok: true,
            status: 200,
            json: () => Promise.resolve(FAKE_DICT),
          } as unknown as Response);
        }
        return Promise.reject(new Error('offline'));
      }),
    );
    const dummy = createApp(Stub);
    await setupI18n(dummy);
  });

  afterEach(() => {
    __resetI18nForTest();
    __resetWorldGeoJsonForTest();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('renders the map card with canvas and legend', async () => {
    const wrapper = await mountWithNodes([]);
    expect(wrapper.find('[data-test="node-map"]').exists()).toBe(true);
    expect(wrapper.find('canvas.map-canvas').exists()).toBe(true);
    expect(wrapper.find('[data-test="map-dots"]').exists()).toBe(true);
  });

  it('renders one positioned dot per node with a status class', async () => {
    const wrapper = await mountWithNodes([
      makeNode({
        identity: { node_id: 'a', node_label: 'A', hostname: 'web-jp-1', tags: [] },
        online: true,
        latency_ms: 20,
      }),
      makeNode({
        identity: { node_id: 'b', node_label: 'B', hostname: 'h', tags: [] },
        online: false,
      }),
    ]);

    const dotEls = wrapper.findAll('[data-test="map-dot"]');
    expect(dotEls).toHaveLength(2);

    expect(dotEls[0]!.classes()).toContain('online');
    expect(dotEls[1]!.classes()).toContain('offline');

    // positioned via inline left/top percentages
    const style = dotEls[0]!.attributes('style') ?? '';
    expect(style).toMatch(/left:\s*[\d.]+%/);
    expect(style).toMatch(/top:\s*[\d.]+%/);
  });

  it('marks a high-latency node with the latency class', async () => {
    const wrapper = await mountWithNodes([
      makeNode({
        identity: { node_id: 'c', node_label: 'C', hostname: 'h', tags: [] },
        online: true,
        latency_ms: 300,
      }),
    ]);
    expect(wrapper.find('[data-test="map-dot"]').classes()).toContain('latency');
  });

  it('repaints the canvas when the theme changes', async () => {
    // getContext is the stubbed entry point of paintWorldDotMap; a repaint
    // calls it again. Mount paints once; toggling theme should repaint.
    const getContext = HTMLCanvasElement.prototype.getContext as unknown as ReturnType<
      typeof vi.fn
    >;
    await mountWithNodes([]);
    const callsAfterMount = getContext.mock.calls.length;
    expect(callsAfterMount).toBeGreaterThan(0);

    useTheme().toggleTheme();
    await nextTick();
    expect(getContext.mock.calls.length).toBeGreaterThan(callsAfterMount);
  });
});
