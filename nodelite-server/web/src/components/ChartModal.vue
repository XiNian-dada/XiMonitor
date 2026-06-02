<script setup lang="ts">
import { computed } from 'vue';
import type { ChartPoint } from '@/lib/chart/chartData';
import type { ChartValueKind } from '@/lib/chart/format';
import type { MultiSeriesInput } from '@/lib/chart/svgModel';
import MetricChart from './MetricChart.vue';

// Visibility is owned by the parent via v-if (it already gates on having a
// selected metric), so there's no `open` prop here — the modal renders its
// content whenever mounted.
const props = defineProps<{
  title: string;
  points?: ChartPoint[];
  series?: MultiSeriesInput[];
  valueKind: ChartValueKind;
  color?: string;
  maxValue?: number;
}>();

const emit = defineEmits<{ close: [] }>();

// Only include points OR series — exactOptionalPropertyTypes forbids passing
// an explicit undefined to an optional prop, so omit the absent one.
const chartProps = computed(() => ({
  valueKind: props.valueKind,
  color: props.color ?? 'var(--accent-blue)',
  label: props.title,
  minValue: 0,
  height: 360,
  ...(props.maxValue !== undefined ? { maxValue: props.maxValue } : {}),
  ...(props.points ? { points: props.points } : {}),
  ...(props.series ? { series: props.series } : {}),
}));
</script>

<template>
  <div
    class="chart-modal"
    data-test="chart-modal"
    role="dialog"
    aria-modal="true"
    @click.self="emit('close')"
  >
    <div class="chart-modal__panel">
      <header class="chart-modal__head">
        <h2 class="chart-modal__title">{{ title }}</h2>
        <button type="button" class="chart-modal__close" data-test="chart-modal-close" @click="emit('close')">
          ✕
        </button>
      </header>
      <MetricChart v-bind="chartProps" />
    </div>
  </div>
</template>

<style scoped>
.chart-modal {
  position: fixed;
  inset: 0;
  background: rgba(2, 6, 16, 0.62);
  display: grid;
  place-items: center;
  padding: 24px;
  z-index: 50;
}
.chart-modal__panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 18px;
  padding: 20px 22px;
  width: min(960px, 100%);
}
.chart-modal__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 14px;
}
.chart-modal__title {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.chart-modal__close {
  width: 32px;
  height: 32px;
  border-radius: 8px;
  border: 1px solid var(--border-soft);
  background: var(--bg-card-soft);
  color: var(--text-muted);
}
.chart-modal__close:hover {
  color: var(--text-primary);
}
</style>
