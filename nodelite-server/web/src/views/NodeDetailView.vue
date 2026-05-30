<script setup lang="ts">
import { computed, onMounted, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import AppLayout from '@/components/AppLayout.vue';
import NodeInfoPanel from '@/components/NodeInfoPanel.vue';
import NodeSummaryCards from '@/components/NodeSummaryCards.vue';
import OverviewCharts from '@/components/OverviewCharts.vue';
import NodeDisks from '@/components/NodeDisks.vue';
import MetricChart from '@/components/MetricChart.vue';
import { usePolling } from '@/composables/usePolling';
import { nodeStatusKey } from '@/lib/map/projection';
import { ipFromNode, locationFromNode } from '@/lib/nodeMeta';
import { uptimeParts, fmtBytes, fmtRate } from '@/lib/format';
import { buildChartData } from '@/lib/chart/chartData';
import { formatChartValue } from '@/lib/chart/format';
import { useI18n } from 'vue-i18n';
import { useNodeStatusStore } from '@/stores/nodeStatus';
import { useDetailHistoryStore } from '@/stores/detailHistory';

const NODE_DETAIL_REFRESH_MS = 5000;

// Tabs the shell renders. `settings` is deferred (Stage 2.5) and rendered
// disabled, mirroring the dashboard sidebar pattern.
const TABS = ['overview', 'monitor', 'network', 'hardware', 'logs'] as const;
type TabId = (typeof TABS)[number];

function isTabId(value: string): value is TabId {
  return (TABS as readonly string[]).includes(value);
}

const route = useRoute();
const router = useRouter();
const { t } = useI18n();
const store = useNodeStatusStore();
const historyStore = useDetailHistoryStore();

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

// Tabs that render history charts; only those trigger the overview-history
// fetch (mirrors legacy detailHistoryNeedsData; monitor lands in 3d).
const historyNeeded = computed(
  () => activeTab.value === 'overview' || activeTab.value === 'network',
);

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
const networkSeries = computed(() => {
  const data = buildChartData(historyStore.points);
  return [
    { label: t('index.node.download'), color: 'var(--chart-network-down)', points: data.dlPts },
    { label: t('index.node.upload'), color: 'var(--chart-network-up)', points: data.upPts },
  ];
});

function ensureHistory(): void {
  // loadIfStale (not load) so re-entering a history tab within the throttle
  // window reuses the cached series, matching legacy fetchOverviewHistory.
  if (historyNeeded.value && nodeId.value) void historyStore.loadIfStale(nodeId.value);
}

onMounted(() => {
  void store.load(nodeId.value);
  ensureHistory();
});

// Navigating between nodes (same component, new :id) reloads both.
watch(nodeId, (id) => {
  if (id) void store.load(id);
  ensureHistory();
});

// Switching into a history tab lazily loads the overview history.
watch(historyNeeded, (needed) => {
  if (needed) ensureHistory();
});

usePolling(() => {
  void store.refresh();
  if (historyNeeded.value) void historyStore.refresh();
}, NODE_DETAIL_REFRESH_MS);
</script>

<template>
  <AppLayout>
    <template #title>
      <div class="node-title" data-test="node-detail-view">
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
        <button
          type="button"
          class="tab-button"
          disabled
          :title="`${$t('node.tabs.settings')} (Stage 2.5)`"
          data-test="tab-settings"
        >
          {{ $t('node.tabs.settings') }}
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
            <MetricChart :series="networkSeries" value-kind="rate" :min-value="0" :height="240" />
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

        <p v-else class="placeholder" data-test="pane-placeholder">
          {{ activeTab }} — coming in Stage 3d
        </p>
      </section>
    </div>
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
@media (max-width: 880px) {
  .overview-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
