<script setup lang="ts">
import { computed, reactive } from 'vue';
import { useI18n } from 'vue-i18n';
import type { HistoryPoint, NodeStatus } from '@/api';
import { buildChartData } from '@/lib/chart/chartData';
import { networkSeries } from '@/lib/chart/svgModel';
import { provideChartHoverGroup } from '@/composables/useChartHoverGroup';
import { PRESET_WINDOWS, type PresetKey } from '@/composables/useChartSelection';
import MetricChart from './MetricChart.vue';

export type MonitorMetric = 'cpu' | 'memory' | 'network' | 'latency';

const props = defineProps<{
  node: NodeStatus | null;
  history: HistoryPoint[];
  activeKey: PresetKey;
}>();

const emit = defineEmits<{
  selectPreset: [key: PresetKey];
  zoom: [metric: MonitorMetric, clipSpikes: boolean];
}>();

const { t } = useI18n();

// The four monitor charts link their crosshairs by timestamp.
provideChartHoverGroup();

const data = computed(() => buildChartData(props.history));
const clipSpikes = reactive<Record<MonitorMetric, boolean>>({
  cpu: true,
  memory: true,
  network: true,
  latency: true,
});

function toggleClip(metric: MonitorMetric): void {
  clipSpikes[metric] = !clipSpikes[metric];
}

// One config per chart; the network entry is multi-series, the rest area.
const charts = computed(() => [
  {
    metric: 'cpu' as const,
    title: t('node.cpu_usage'),
    chartProps: {
      points: data.value.cpuPts,
      valueKind: 'percent' as const,
      color: 'var(--chart-cpu)',
      label: t('node.cpu_usage'),
      clipSpikes: clipSpikes.cpu,
    },
  },
  {
    metric: 'memory' as const,
    title: t('node.memory_usage'),
    chartProps: {
      points: data.value.memPts,
      valueKind: 'percent' as const,
      color: 'var(--chart-memory)',
      label: t('node.memory_usage'),
      maxValue: 100,
      clipSpikes: clipSpikes.memory,
    },
  },
  {
    metric: 'network' as const,
    title: t('node.network_traffic'),
    chartProps: {
      series: networkSeries(data.value, t('index.node.download'), t('index.node.upload')),
      valueKind: 'rate' as const,
      clipSpikes: clipSpikes.network,
    },
  },
  {
    metric: 'latency' as const,
    title: t('node.latency_history'),
    chartProps: {
      points: data.value.rttPts,
      valueKind: 'latency' as const,
      color: 'var(--chart-latency)',
      label: t('node.latency_history'),
      clipSpikes: clipSpikes.latency,
    },
  },
]);
</script>

<template>
  <div class="monitor" data-test="monitor-charts">
    <div class="monitor__presets" role="group" data-test="monitor-presets">
      <button
        v-for="preset in PRESET_WINDOWS"
        :key="preset.key"
        type="button"
        class="preset-button"
        :class="{ active: activeKey === preset.key }"
        :data-test="`preset-${preset.key}`"
        @click="emit('selectPreset', preset.key)"
      >
        {{ t(`node.preset.${preset.key}`) }}
      </button>
    </div>

    <div class="monitor__grid">
      <article v-for="chart in charts" :key="chart.metric" class="panel big-chart">
        <header class="big-chart__head">
          <span class="big-chart__title">{{ chart.title }}</span>
          <div class="big-chart__actions">
            <button
              type="button"
              class="chart-clip-toggle"
              :class="{ active: clipSpikes[chart.metric] }"
              :aria-label="clipSpikes[chart.metric] ? t('node.clip.on') : t('node.clip.off')"
              :aria-pressed="clipSpikes[chart.metric]"
              :title="clipSpikes[chart.metric] ? t('node.clip.on') : t('node.clip.off')"
              :data-test="`clip-${chart.metric}`"
              @click="toggleClip(chart.metric)"
            >
              {{ clipSpikes[chart.metric] ? t('node.clip.on_short') : t('node.clip.off_short') }}
            </button>
            <button
              type="button"
              class="zoom-button"
              :aria-label="t('node.chart.zoom')"
              :title="t('node.chart.zoom')"
              :data-test="`zoom-${chart.metric}`"
              @click="emit('zoom', chart.metric, clipSpikes[chart.metric])"
            >
              ⤢
            </button>
          </div>
        </header>
        <MetricChart v-bind="chart.chartProps" :min-value="0" :height="220" />
      </article>
    </div>
  </div>
</template>

<style scoped>
.monitor__presets {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 14px;
}
.preset-button {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 999px;
  color: var(--text-secondary);
  padding: 6px 14px;
  font-size: 12px;
}
.preset-button:hover {
  border-color: var(--border-strong);
}
.preset-button.active {
  color: var(--accent-blue);
  border-color: var(--accent-blue);
  background: var(--accent-blue-soft);
}
.monitor__grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
  gap: 14px;
}
.big-chart {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 16px 18px;
}
.big-chart__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}
.big-chart__title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}
.big-chart__actions {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex: 0 0 auto;
}
.chart-clip-toggle {
  border: 1px solid var(--border-soft);
  background: var(--bg-card-soft);
  color: var(--text-muted);
  border-radius: 999px;
  padding: 4px 9px;
  font-size: 11px;
  font-weight: 500;
  white-space: nowrap;
}
.chart-clip-toggle.active {
  background: var(--accent-blue-soft);
  color: var(--accent-blue);
  border-color: rgba(59, 130, 246, 0.32);
}
.chart-clip-toggle:hover {
  color: var(--text-primary);
}
.chart-clip-toggle.active:hover {
  color: var(--accent-blue);
}
.zoom-button {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  border: 1px solid var(--border-soft);
  background: var(--bg-card-soft);
  color: var(--text-muted);
}
.zoom-button:hover {
  color: var(--text-primary);
}
</style>
