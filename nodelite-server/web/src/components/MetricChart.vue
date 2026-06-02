<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useI18n } from 'vue-i18n';
import type { ChartPoint } from '@/lib/chart/chartData';
import type { ChartValueKind } from '@/lib/chart/format';
import {
  buildAreaChart,
  buildMultiAreaChart,
  type MultiSeriesInput,
} from '@/lib/chart/svgModel';
import { useChart } from '@/composables/useChart';

/*
 * points/series/minValue/maxValue are intentionally default-less: `undefined`
 * is the meaningful "not provided" (series presence selects area vs multi
 * mode; a min/max default would distort auto-scaling). An array factory
 * default would also break withDefaults' typing here.
 */
/* eslint-disable vue/require-default-prop */
const props = withDefaults(
  defineProps<{
    /** Single-series area chart points (the default variant). */
    points?: ChartPoint[];
    /** Multi-series lines (network down/up). Takes precedence over points. */
    series?: MultiSeriesInput[];
    valueKind?: ChartValueKind;
    color?: string;
    label?: string;
    clipSpikes?: boolean;
    minValue?: number;
    maxValue?: number;
    height?: number;
  }>(),
  {
    valueKind: 'number',
    color: 'var(--accent-blue)',
    label: '',
    clipSpikes: true,
    height: 180,
  },
);
/* eslint-enable vue/require-default-prop */

const { locale, t } = useI18n();
const containerRef = ref<HTMLElement | null>(null);
const gradId = `metric-grad-${useId()}`;

const isMulti = computed(() => Array.isArray(props.series));

function axisTimeLabel(ts: number): string {
  return new Date(ts).toLocaleTimeString(locale.value, {
    hour: '2-digit',
    minute: '2-digit',
  });
}

const { model, hover, onPointerMove, onPointerLeave } = useChart(
  containerRef,
  ({ width, height }) => {
    const base = {
      width,
      height,
      valueKind: props.valueKind,
      clipSpikes: props.clipSpikes,
      ...(props.minValue !== undefined ? { minValue: props.minValue } : {}),
      ...(props.maxValue !== undefined ? { maxValue: props.maxValue } : {}),
    };
    if (props.series) {
      return buildMultiAreaChart(props.series, base);
    }
    return buildAreaChart(props.points ?? [], {
      ...base,
      color: props.color,
      label: props.label,
    });
  },
  {
    fallbackHeight: props.height,
    formatTime: (ts) => new Date(ts).toLocaleString(locale.value),
  },
);
</script>

<template>
  <div
    ref="containerRef"
    class="metric-chart"
    :style="{ minHeight: `${height}px` }"
    data-test="metric-chart"
    @pointermove="onPointerMove"
    @pointerleave="onPointerLeave"
  >
    <div v-if="model.empty" class="metric-chart__empty" data-test="metric-chart-empty">
      {{ t('node.waiting_history') }}
    </div>

    <template v-else>
      <svg
        :viewBox="`0 0 ${model.width} ${model.height}`"
        preserveAspectRatio="xMinYMin meet"
        class="metric-chart__svg"
        aria-hidden="true"
        data-test="metric-chart-svg"
      >
        <defs v-if="!isMulti">
          <linearGradient :id="gradId" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" :stop-color="color" stop-opacity="0.32" />
            <stop offset="100%" :stop-color="color" stop-opacity="0" />
          </linearGradient>
        </defs>

        <g class="metric-chart__grid">
          <template v-for="(g, i) in model.grid" :key="i">
            <line
              :x1="model.padLeft"
              :x2="model.width - model.padRight"
              :y1="g.y"
              :y2="g.y"
              stroke="currentColor"
              stroke-opacity="0.09"
              stroke-width="1"
            />
            <text
              :x="model.padLeft - 8"
              :y="g.y + 3"
              text-anchor="end"
              fill="currentColor"
              opacity="0.64"
              font-size="11"
            >
              {{ g.label }}
            </text>
          </template>
        </g>

        <g v-if="model.timeTicks.length > 0" class="metric-chart__x-axis">
          <text
            v-for="tick in model.timeTicks"
            :key="tick.ts"
            :x="tick.x"
            :y="model.height - 8"
            text-anchor="middle"
            fill="currentColor"
            opacity="0.54"
            font-size="10"
            data-test="metric-chart-x-tick"
          >
            {{ axisTimeLabel(tick.ts) }}
          </text>
        </g>

        <template v-for="(s, i) in model.series" :key="i">
          <path
            v-if="s.area"
            :d="s.area"
            :fill="`url(#${gradId})`"
            data-test="metric-chart-area"
          />
          <line
            v-if="s.avgY !== null"
            :x1="model.padLeft"
            :x2="model.width - model.padRight"
            :y1="s.avgY"
            :y2="s.avgY"
            :stroke="s.color"
            :stroke-opacity="s.avgOpacity"
            stroke-width="1.2"
            stroke-dasharray="6 5"
          />
          <path
            :d="s.line"
            fill="none"
            :stroke="s.color"
            stroke-width="1.45"
            stroke-linecap="round"
            stroke-linejoin="round"
            vector-effect="non-scaling-stroke"
            data-test="metric-chart-line"
          />
        </template>

        <g v-if="hover" class="metric-chart__hover">
          <line
            :x1="hover.lineX"
            :x2="hover.lineX"
            :y1="hover.lineY1"
            :y2="hover.lineY2"
            stroke="currentColor"
            stroke-opacity="0.46"
            stroke-width="1"
          />
          <circle
            v-for="(c, i) in hover.circles"
            :key="i"
            :cx="c.cx"
            :cy="c.cy"
            r="4"
            stroke-width="2"
            :stroke="c.color"
            fill="var(--bg-card)"
          />
        </g>
      </svg>

      <div
        v-if="hover"
        class="chart-tooltip"
        data-test="metric-chart-tooltip"
        :style="{ left: `${hover.tooltip.left}px`, top: `${hover.tooltip.top}px`, transform: hover.tooltip.transform }"
      >
        <div class="chart-tooltip__time">{{ hover.tooltip.time }}</div>
        <div v-for="(row, i) in hover.tooltip.rows" :key="i" class="chart-tooltip__row">
          <span>
            <span class="chart-tooltip__swatch" :style="{ background: row.color }" />
            {{ row.label }}
          </span>
          <strong>{{ row.value }}</strong>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.metric-chart {
  position: relative;
  width: 100%;
  color: var(--text-muted);
}
.metric-chart__svg {
  display: block;
  width: 100%;
  height: 100%;
}
.metric-chart__empty {
  padding: 20px;
  color: var(--text-muted);
  font-size: 13px;
}
.metric-chart__x-axis {
  pointer-events: none;
  font-variant-numeric: tabular-nums;
}
.chart-tooltip {
  position: absolute;
  pointer-events: none;
  background: var(--bg-elevated);
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  padding: 9px 11px;
  font-size: 11px;
  line-height: 1.45;
  min-width: 138px;
  max-width: 220px;
  z-index: 5;
  color: var(--text-primary);
  font-variant-numeric: tabular-nums;
}
.chart-tooltip__time {
  color: var(--text-muted);
  margin-bottom: 5px;
  white-space: nowrap;
}
.chart-tooltip__row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.chart-tooltip__row > span {
  display: inline-flex;
  align-items: center;
  min-width: 0;
  white-space: nowrap;
}
.chart-tooltip__row strong {
  flex: 0 0 auto;
  text-align: right;
  white-space: nowrap;
}
.chart-tooltip__swatch {
  display: inline-block;
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  margin-right: 5px;
}
</style>
