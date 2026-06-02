<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { HistoryPoint, NodeStatus } from '@/api';
import { buildChartData, averageValue, type ChartPoint } from '@/lib/chart/chartData';
import { networkSeries } from '@/lib/chart/svgModel';
import { formatChartValue, type ChartValueKind } from '@/lib/chart/format';
import { provideChartHoverGroup } from '@/composables/useChartHoverGroup';
import MetricChart from './MetricChart.vue';

const props = defineProps<{ node: NodeStatus | null; history: HistoryPoint[] }>();

const { t } = useI18n();

// The four overview charts share a hover group → crosshairs link by timestamp.
provideChartHoverGroup();

const data = computed(() => buildChartData(props.history));

function avgText(pts: ChartPoint[], kind: ChartValueKind): string {
  const avg = averageValue(pts);
  if (avg == null) return t('node.chart.average', { value: '—' });
  return t('node.chart.average', { value: formatChartValue(avg, kind) });
}

const nowCpu = computed(() =>
  props.node?.snapshot?.cpu_usage_percent == null
    ? '—'
    : formatChartValue(props.node.snapshot.cpu_usage_percent, 'percent'),
);
const nowMemory = computed(() => {
  const mem = props.node?.snapshot?.memory;
  return mem?.total_bytes ? formatChartValue((mem.used_bytes / mem.total_bytes) * 100, 'percent') : '—';
});
const nowLatency = computed(() =>
  props.node?.latency_ms == null ? '—' : formatChartValue(props.node.latency_ms, 'latency'),
);

const netSeries = computed(() =>
  networkSeries(data.value, t('index.node.download'), t('index.node.upload')),
);
</script>

<template>
  <div class="overview-charts" data-test="overview-charts">
    <article class="panel chart-card">
      <header class="chart-card__head">
        <span class="chart-card__title">{{ t('node.cpu_usage') }}</span>
        <span class="chart-card__meta">
          <strong data-test="now-cpu">{{ nowCpu }}</strong>
          <small>{{ avgText(data.cpuPts, 'percent') }}</small>
        </span>
      </header>
      <MetricChart
        :points="data.cpuPts"
        value-kind="percent"
        color="var(--chart-cpu)"
        :label="t('node.cpu_usage')"
        :min-value="0"
      />
    </article>

    <article class="panel chart-card">
      <header class="chart-card__head">
        <span class="chart-card__title">{{ t('node.memory_usage') }}</span>
        <span class="chart-card__meta">
          <strong data-test="now-memory">{{ nowMemory }}</strong>
          <small>{{ avgText(data.memPts, 'percent') }}</small>
        </span>
      </header>
      <MetricChart
        :points="data.memPts"
        value-kind="percent"
        color="var(--chart-memory)"
        :label="t('node.memory_usage')"
        :min-value="0"
        :max-value="100"
      />
    </article>

    <article class="panel chart-card">
      <header class="chart-card__head">
        <span class="chart-card__title">{{ t('node.network_traffic') }}</span>
      </header>
      <MetricChart :series="netSeries" value-kind="rate" :min-value="0" />
    </article>

    <article class="panel chart-card chart-card--rtt">
      <header class="chart-card__head">
        <span class="chart-card__title">{{ t('node.latency_history') }}</span>
        <span class="chart-card__meta">
          <strong data-test="now-rtt">{{ nowLatency }}</strong>
          <small>{{ avgText(data.rttPts, 'latency') }}</small>
        </span>
      </header>
      <MetricChart
        :points="data.rttPts"
        value-kind="latency"
        color="var(--chart-latency)"
        :label="t('node.latency_history')"
        :min-value="0"
        :height="96"
      />
    </article>
  </div>
</template>

<style scoped>
.overview-charts {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  gap: 14px;
}
.chart-card {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 16px 18px;
}
.chart-card__head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}
.chart-card__title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}
.chart-card__meta {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
}
.chart-card__meta strong {
  font-size: 15px;
  font-variant-numeric: tabular-nums;
}
.chart-card__meta small {
  color: var(--text-muted);
  font-size: 11px;
}
</style>
