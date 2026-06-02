<script setup lang="ts">
import { computed } from 'vue';
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
  zoom: [metric: MonitorMetric];
}>();

const { t } = useI18n();

// The four monitor charts link their crosshairs by timestamp.
provideChartHoverGroup();

const data = computed(() => buildChartData(props.history));

// One config per chart; the network entry is multi-series, the rest area.
const charts = computed(() => [
  {
    metric: 'cpu' as const,
    title: t('node.cpu_usage'),
    chartProps: { points: data.value.cpuPts, valueKind: 'percent' as const, color: 'var(--chart-cpu)', label: t('node.cpu_usage') },
  },
  {
    metric: 'memory' as const,
    title: t('node.memory_usage'),
    chartProps: { points: data.value.memPts, valueKind: 'percent' as const, color: 'var(--chart-memory)', label: t('node.memory_usage'), maxValue: 100 },
  },
  {
    metric: 'network' as const,
    title: t('node.network_traffic'),
    chartProps: { series: networkSeries(data.value, t('index.node.download'), t('index.node.upload')), valueKind: 'rate' as const },
  },
  {
    metric: 'latency' as const,
    title: t('node.latency_history'),
    chartProps: { points: data.value.rttPts, valueKind: 'latency' as const, color: 'var(--chart-latency)', label: t('node.latency_history') },
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
          <button
            type="button"
            class="zoom-button"
            :aria-label="t('node.chart.zoom')"
            :title="t('node.chart.zoom')"
            :data-test="`zoom-${chart.metric}`"
            @click="emit('zoom', chart.metric)"
          >
            ⤢
          </button>
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
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
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
  margin-bottom: 10px;
}
.big-chart__title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
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
