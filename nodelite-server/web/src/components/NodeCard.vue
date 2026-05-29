<script setup lang="ts">
import { computed, watch } from 'vue';
import type { NodeListItem } from '@/api';
import { nodeFlag, nodeStatusKey } from '@/lib/map/projection';
import { buildSparkline, nodeSparkPoints, sparklineColor } from '@/lib/chart/sparkline';
import { useNodeHistoryStore } from '@/stores/nodeHistory';

const props = defineProps<{ node: NodeListItem }>();

const historyStore = useNodeHistoryStore();

const nodeId = computed(() => props.node.identity.node_id);
const status = computed(() => nodeStatusKey(props.node));

const title = computed(() => {
  const { node_label: label, node_id: id } = props.node.identity;
  return label && label !== id ? `${label}: ${id}` : id;
});

const statusLabelKey = computed(() => {
  switch (status.value) {
    case 'offline':
      return 'common.offline';
    case 'latency':
      return 'common.latency_warn';
    default:
      return 'common.online';
  }
});

const latencyText = computed(() =>
  props.node.latency_ms == null ? '—' : `${Math.round(props.node.latency_ms)} ms`,
);

const loadText = computed(() => {
  const one = props.node.snapshot?.load.one;
  return one == null ? '—' : one.toFixed(2);
});

const cpu = computed(() => props.node.snapshot?.cpu_usage_percent ?? null);
const cpuText = computed(() => (cpu.value == null ? '—' : `${cpu.value.toFixed(0)}%`));
const cpuClass = computed(() => {
  const v = cpu.value;
  if (v == null) return '';
  if (v >= 80) return 'accent-red';
  if (v >= 50) return 'accent-yellow';
  return 'accent-green';
});

const sparkColor = computed(() => sparklineColor(status.value));
const sparkPoints = computed(() =>
  nodeSparkPoints(historyStore.points(nodeId.value), cpu.value),
);
const spark = computed(() => buildSparkline(sparkPoints.value));

// Re-request on every snapshot change (the 5s poll replaces node objects),
// throttled to once a minute by the store's TTL. NodeCard is keyed by
// node_id so the instance is reused across polls — onMounted alone would
// fire only once and freeze the sparkline.
watch(() => props.node.snapshot, () => void historyStore.loadIfStale(nodeId.value), {
  immediate: true,
});
</script>

<template>
  <RouterLink
    class="node-card"
    :to="`/nodes/${encodeURIComponent(nodeId)}`"
    data-test="node-card"
    :data-node-id="nodeId"
  >
    <div class="node-card-head">
      <div class="node-card-title">
        <span class="flag">{{ nodeFlag(node) }}</span>
        <span :title="title">{{ title }}</span>
      </div>
      <span class="badge" :class="status" data-test="node-badge">
        {{ $t(statusLabelKey) }}
      </span>
    </div>

    <div class="node-metrics">
      <div class="node-metric">
        <div class="label">{{ $t('index.node.latency') }}</div>
        <div class="value" data-test="metric-latency">{{ latencyText }}</div>
      </div>
      <div class="node-metric">
        <div class="label">{{ $t('index.node.load') }}</div>
        <div class="value" data-test="metric-load">{{ loadText }}</div>
      </div>
      <div class="node-metric">
        <div class="label">{{ $t('index.node.cpu') }}</div>
        <div class="value" :class="cpuClass" data-test="metric-cpu">{{ cpuText }}</div>
      </div>
    </div>

    <div class="node-spark" :style="{ color: sparkColor }">
      <svg
        v-if="spark"
        :viewBox="`0 0 ${spark.width} ${spark.height}`"
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        <path :d="spark.area" :fill="sparkColor" fill-opacity="0.16" />
        <path
          :d="spark.line"
          fill="none"
          :stroke="sparkColor"
          stroke-width="1.1"
          stroke-linecap="round"
          stroke-linejoin="round"
          vector-effect="non-scaling-stroke"
        />
      </svg>
      <svg v-else viewBox="0 0 200 60" preserveAspectRatio="none" aria-hidden="true">
        <line x1="0" y1="48" x2="200" y2="48" :stroke="sparkColor" stroke-width="1" stroke-opacity="0.28" />
      </svg>
    </div>
  </RouterLink>
</template>

<style scoped>
.node-card {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 14px;
  padding: 14px 16px 0;
  display: flex;
  flex-direction: column;
  min-height: 168px;
  transition:
    transform 160ms ease,
    border-color 160ms ease;
  overflow: hidden;
}
.node-card:hover {
  border-color: var(--border-strong);
  transform: translateY(-1px);
}
.node-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.node-card-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
  font-size: 14px;
  color: var(--text-primary);
  min-width: 0;
}
.node-card-title .flag {
  font-size: 18px;
  line-height: 1;
}
.node-card-title span:last-child {
  text-overflow: ellipsis;
  overflow: hidden;
  white-space: nowrap;
}
.badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  font-weight: 500;
  padding: 4px 8px;
  border-radius: 999px;
  background: var(--bg-card-soft);
  color: var(--text-muted);
  white-space: nowrap;
}
.badge::before {
  content: '';
  display: inline-block;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}
.badge.online {
  color: var(--accent-green);
  background: var(--accent-green-soft);
}
.badge.latency {
  color: var(--accent-yellow);
  background: var(--accent-yellow-soft);
}
.badge.offline {
  color: var(--accent-red);
  background: var(--accent-red-soft);
}
.node-metrics {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
  margin: 12px 0 4px;
}
.node-metric .label {
  font-size: 11px;
  color: var(--text-muted);
  margin-bottom: 2px;
}
.node-metric .value {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
  font-variant-numeric: tabular-nums;
}
.node-metric .value.accent-green {
  color: var(--accent-green);
}
.node-metric .value.accent-yellow {
  color: var(--accent-yellow);
}
.node-metric .value.accent-red {
  color: var(--accent-red);
}
.node-spark {
  height: 52px;
  margin: 8px -16px -2px;
  position: relative;
}
.node-spark svg {
  width: 100%;
  height: 100%;
  display: block;
}
</style>
