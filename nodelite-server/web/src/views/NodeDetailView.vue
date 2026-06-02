<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import AppLayout from '@/components/AppLayout.vue';
import NodeInfoPanel from '@/components/NodeInfoPanel.vue';
import NodeSummaryCards from '@/components/NodeSummaryCards.vue';
import OverviewCharts from '@/components/OverviewCharts.vue';
import NodeDisks from '@/components/NodeDisks.vue';
import MetricChart from '@/components/MetricChart.vue';
import MonitorCharts, { type MonitorMetric } from '@/components/MonitorCharts.vue';
import ChartModal from '@/components/ChartModal.vue';
import LogPanel from '@/components/LogPanel.vue';
import NodeSettingsPanel from '@/components/NodeSettingsPanel.vue';
import { usePolling } from '@/composables/usePolling';
import { useChartSelection, type PresetKey } from '@/composables/useChartSelection';
import { nodeStatusKey } from '@/lib/map/projection';
import { ipFromNode, locationFromNode } from '@/lib/nodeMeta';
import { uptimeParts, fmtBytes, fmtRate } from '@/lib/format';
import { buildChartData } from '@/lib/chart/chartData';
import { networkSeries } from '@/lib/chart/svgModel';
import { formatChartValue } from '@/lib/chart/format';
import { useI18n } from 'vue-i18n';
import { useNodeStatusStore } from '@/stores/nodeStatus';
import { useDetailHistoryStore } from '@/stores/detailHistory';
import { useMonitorHistoryStore } from '@/stores/monitorHistory';
import { useNodeLogsStore } from '@/stores/nodeLogs';
import { ApiError } from '@/api/client';

const NODE_DETAIL_REFRESH_MS = 5000;

const TABS = ['overview', 'monitor', 'network', 'hardware', 'logs', 'settings'] as const;
type TabId = (typeof TABS)[number];

function isTabId(value: string): value is TabId {
  return (TABS as readonly string[]).includes(value);
}

const route = useRoute();
const router = useRouter();
const { t } = useI18n();
const store = useNodeStatusStore();
const historyStore = useDetailHistoryStore();
const monitorStore = useMonitorHistoryStore();
const logsStore = useNodeLogsStore();
const selection = useChartSelection();

const nodeId = computed(() => String(route.params.id ?? ''));
const node = computed(() => store.data);

// Active tab is driven by the URL hash (e.g. /nodes/x#monitor), matching the
// legacy hash sync; falls back to overview.
const activeTab = computed<TabId>(() => {
  const hash = route.hash.replace(/^#/, '');
  return isTabId(hash) ? hash : 'overview';
});

function selectTab(tab: TabId): void {
  void router.replace({ hash: `#${tab}` });
}

const status = computed(() => (node.value ? nodeStatusKey(node.value) : 'offline'));
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

const title = computed(
  () => node.value?.identity.node_label || node.value?.identity.node_id || nodeId.value,
);
const ip = computed(() => (node.value ? ipFromNode(node.value) : null));
const location = computed(() => (node.value ? locationFromNode(node.value) : null));
const uptime = computed(() => uptimeParts(node.value?.snapshot?.uptime_secs));

// Render not-found state only when the API returned 404. Other errors (500,
// network failure, JSON parse error) should show a generic error/retry state.
const notFound = computed(
  () => store.error instanceof ApiError && store.error.status === 404 && store.data === null,
);
// Generic error state for non-404 failures (network, 500, etc).
const loadError = computed(
  () => store.error !== null && store.data === null && !notFound.value,
);

// Tabs that render the overview-history charts (overview/network).
const historyNeeded = computed(
  () => activeTab.value === 'overview' || activeTab.value === 'network',
);
const monitorNeeded = computed(() => activeTab.value === 'monitor');
const logsNeeded = computed(() => activeTab.value === 'logs');

// Network tab values (legacy renderSummaryCards net block).
const net = computed(() => {
  const n = node.value?.snapshot?.network;
  return {
    downRate: fmtRate(n?.rx_bytes_per_sec) ?? '—',
    upRate: fmtRate(n?.tx_bytes_per_sec) ?? '—',
    downTotal: fmtBytes(n?.total_rx_bytes) ?? '—',
    upTotal: fmtBytes(n?.total_tx_bytes) ?? '—',
    latency: node.value?.latency_ms == null ? '—' : formatChartValue(node.value.latency_ms, 'latency'),
  };
});
const netSeries = computed(() =>
  networkSeries(buildChartData(historyStore.points), t('index.node.download'), t('index.node.upload')),
);

// loadIfStale (not load) so re-entering a tab within the throttle window
// reuses the cached data, matching legacy fetchOverviewHistory/fetchAgentLogs.
function ensureTabData(): void {
  const id = nodeId.value;
  if (!id) return;
  if (historyNeeded.value) void historyStore.loadIfStale(id);
  if (monitorNeeded.value) void monitorStore.loadIfStale(id, selection.windowHours.value);
  if (logsNeeded.value) void logsStore.loadIfStale(id);
}

onMounted(() => {
  void store.load(nodeId.value);
  ensureTabData();
});

// Navigating between nodes (same component, new :id) reloads.
watch(nodeId, (id) => {
  if (id) void store.load(id);
  ensureTabData();
});

// Switching tabs / changing the monitor window lazily loads that data.
watch([activeTab, selection.windowHours], () => ensureTabData());

usePolling(() => {
  void store.refresh();
  if (historyNeeded.value) void historyStore.refresh();
  if (monitorNeeded.value && nodeId.value) {
    void monitorStore.refresh(nodeId.value, selection.windowHours.value);
  }
  if (logsNeeded.value) void logsStore.refresh();
}, NODE_DETAIL_REFRESH_MS);

// --- Monitor zoom modal ---
const modalMetric = ref<MonitorMetric | null>(null);
function openZoom(metric: MonitorMetric): void {
  modalMetric.value = metric;
}
function closeZoom(): void {
  modalMetric.value = null;
}
function onSelectPreset(key: PresetKey): void {
  selection.selectPreset(key);
}

const modalConfig = computed(() => {
  const metric = modalMetric.value;
  if (!metric) return null;
  const data = buildChartData(monitorStore.points);
  switch (metric) {
    case 'cpu':
      return { title: t('node.cpu_usage'), points: data.cpuPts, valueKind: 'percent' as const, color: 'var(--chart-cpu)' };
    case 'memory':
      return { title: t('node.memory_usage'), points: data.memPts, valueKind: 'percent' as const, color: 'var(--chart-memory)', maxValue: 100 };
    case 'latency':
      return { title: t('node.latency_history'), points: data.rttPts, valueKind: 'latency' as const, color: 'var(--chart-latency)' };
    case 'network':
      return {
        title: t('node.network_traffic'),
        series: networkSeries(data, t('index.node.download'), t('index.node.upload')),
        valueKind: 'rate' as const,
      };
  }
  return null;
});
</script>

<template>
  <AppLayout>
    <template #title>
      <!-- Hide header when showing not-found or generic error state -->
      <div v-if="!notFound && !loadError" class="node-title" data-test="node-detail-view">
        <h1 class="node-title__name">{{ title }}</h1>
        <span class="badge" :class="status" data-test="node-status-badge">
          {{ $t(statusLabelKey) }}
        </span>
        <div class="node-title__meta" data-test="node-meta">
          <span v-if="ip">{{ $t('node.meta.ip', { ip }) }}</span>
          <span v-if="location">{{ location }}</span>
          <span v-if="uptime && uptime.days > 0">{{ $t('node.meta.uptime_days', { days: uptime.days }) }}</span>
          <span v-else-if="uptime">{{ $t('node.meta.uptime_hours', { hours: uptime.hours }) }}</span>
        </div>
      </div>
    </template>

    <div class="node-detail">
      <!-- Not-found state: API returned 404 -->
      <div v-if="notFound" class="error-state" data-test="node-not-found">
        <div class="error-state__icon">⚠️</div>
        <h2 class="error-state__title">{{ $t('node.not_found.title') }}</h2>
        <p class="error-state__message">
          {{ $t('node.not_found.message', { nodeId: nodeId }) }}
        </p>
        <RouterLink to="/" class="error-state__link">
          {{ $t('node.not_found.back_to_dashboard') }}
        </RouterLink>
      </div>

      <!-- Generic error state: network failure, 500, etc -->
      <div v-else-if="loadError" class="error-state" data-test="node-load-error">
        <div class="error-state__icon">⚠️</div>
        <h2 class="error-state__title">{{ $t('node.load_error.title') }}</h2>
        <p class="error-state__message">
          {{ $t('node.load_error.message') }}
        </p>
        <button type="button" class="error-state__button" @click="store.refresh()">
          {{ $t('node.load_error.retry') }}
        </button>
      </div>

      <template v-else>
        <nav class="tabs" data-test="node-tabs">
          <button
            v-for="tab in TABS"
            :key="tab"
            type="button"
            class="tab-button"
            :class="{ active: activeTab === tab }"
            :data-test="`tab-${tab}`"
            @click="selectTab(tab)"
          >
            {{ $t(`node.tabs.${tab}`) }}
          </button>
        </nav>

        <section class="tab-pane" :data-pane="activeTab" data-test="node-tab-pane">
        <template v-if="activeTab === 'overview'">
          <div class="overview-grid">
            <NodeInfoPanel :node="node" />
            <NodeSummaryCards :node="node" />
          </div>
          <OverviewCharts :node="node" :history="historyStore.points" />
        </template>

        <template v-else-if="activeTab === 'network'">
          <div class="net-stats" data-test="network-pane">
            <div class="net-stat">
              <span class="net-stat__label">↓ {{ $t('index.node.download') }}</span>
              <strong>{{ net.downRate }}</strong>
              <small>total {{ net.downTotal }}</small>
            </div>
            <div class="net-stat">
              <span class="net-stat__label">↑ {{ $t('index.node.upload') }}</span>
              <strong>{{ net.upRate }}</strong>
              <small>total {{ net.upTotal }}</small>
            </div>
            <div class="net-stat">
              <span class="net-stat__label">{{ $t('node.latency_history') }}</span>
              <strong>{{ net.latency }}</strong>
            </div>
          </div>
          <article class="panel">
            <MetricChart :series="netSeries" value-kind="rate" :min-value="0" :height="240" />
          </article>
        </template>

        <template v-else-if="activeTab === 'hardware'">
          <div class="overview-grid">
            <NodeInfoPanel :node="node" />
          </div>
          <article class="panel">
            <NodeDisks :node="node" />
          </article>
        </template>

        <MonitorCharts
          v-else-if="activeTab === 'monitor'"
          :node="node"
          :history="monitorStore.points"
          :active-key="selection.activeKey.value"
          @select-preset="onSelectPreset"
          @zoom="openZoom"
        />

        <LogPanel
          v-else-if="activeTab === 'logs'"
          :entries="logsStore.entries"
          :error="logsStore.error"
        />

        <NodeSettingsPanel v-else-if="activeTab === 'settings'" :node-id="nodeId" />
      </section>
      </template>
    </div>

    <ChartModal v-if="modalConfig" v-bind="modalConfig" @close="closeZoom" />
  </AppLayout>
</template>

<style scoped>
.node-title {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.node-title__name {
  margin: 0;
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.node-title__meta {
  display: flex;
  gap: 12px;
  color: var(--text-muted);
  font-size: 13px;
  width: 100%;
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
.tabs {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
  border-bottom: 1px solid var(--border-soft);
  margin-bottom: 18px;
}
.tab-button {
  background: transparent;
  border: 0;
  border-bottom: 2px solid transparent;
  color: var(--text-muted);
  padding: 8px 14px;
  font-size: 13px;
  font-weight: 500;
}
.tab-button:hover:not([disabled]) {
  color: var(--text-secondary);
}
.tab-button.active {
  color: var(--accent-blue);
  border-bottom-color: var(--accent-blue);
}
.tab-button[disabled] {
  opacity: 0.4;
  cursor: not-allowed;
}
.placeholder {
  color: var(--text-muted);
  font-size: 13px;
}
.overview-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 2fr);
  gap: 14px;
  margin-bottom: 14px;
  align-items: start;
}
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 16px 18px;
}
.net-stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 14px;
  margin-bottom: 14px;
}
.net-stat {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.net-stat__label {
  color: var(--text-muted);
  font-size: 12px;
}
.net-stat strong {
  font-size: 20px;
  font-variant-numeric: tabular-nums;
}
.net-stat small {
  color: var(--text-muted);
  font-size: 12px;
}
.error-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 80px 20px;
  text-align: center;
  min-height: 400px;
}
.error-state__icon {
  font-size: 64px;
  margin-bottom: 20px;
  opacity: 0.6;
}
.error-state__title {
  margin: 0 0 12px;
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
}
.error-state__message {
  margin: 0 0 24px;
  font-size: 14px;
  color: var(--text-muted);
  max-width: 400px;
}
.error-state__link {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 10px 20px;
  background: var(--accent-blue);
  color: white;
  border-radius: 8px;
  text-decoration: none;
  font-size: 14px;
  font-weight: 500;
  transition: opacity 0.2s;
}
.error-state__link:hover {
  opacity: 0.9;
}
.error-state__button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 10px 20px;
  background: var(--accent-blue);
  color: white;
  border: none;
  border-radius: 8px;
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: opacity 0.2s;
}
.error-state__button:hover {
  opacity: 0.9;
}
@media (max-width: 880px) {
  .overview-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
