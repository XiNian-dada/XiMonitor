<script setup lang="ts">
import { computed, onMounted, onUnmounted } from 'vue';
import AppLayout from '@/components/AppLayout.vue';
import OverviewStats from '@/components/OverviewStats.vue';
import NodeMap from '@/components/NodeMap.vue';
import NodeList from '@/components/NodeList.vue';
import { useWebSocket } from '@/ws';
import { useBootstrapStore } from '@/stores/bootstrap';
import { useOverviewStore } from '@/stores/overview';
import { useNodesStore } from '@/stores/nodes';

const bootstrapStore = useBootstrapStore();
const overviewStore = useOverviewStore();
const nodesStore = useNodesStore();
const ws = useWebSocket();

const onlineCount = computed(() => overviewStore.data?.online_nodes ?? 0);

onMounted(() => {
  void bootstrapStore.load();

  // WS-first: subscribe to WebSocket messages
  const offInitial = ws.on('initial_state', (msg) => {
    overviewStore.apply(msg.overview, msg.generated_at);
    nodesStore.applyServerState(msg.nodes, msg.generated_at);
  });

  const offOverview = ws.on('overview_update', (msg) => {
    overviewStore.apply(msg.overview, msg.generated_at);
  });

  const offUpsert = ws.on('node_upsert', (msg) => {
    nodesStore.upsertNode(msg.node, msg.generated_at);
  });

  const offRemoved = ws.on('node_removed', (msg) => {
    nodesStore.removeNode(msg.node_id, msg.generated_at);
  });

  // Fallback: if WS hasn't delivered InitialState within 3s, fetch via REST
  const fallbackTimer = window.setTimeout(() => {
    if (!nodesStore.lastGeneratedAt) {
      void Promise.all([overviewStore.refresh(), nodesStore.refresh()]);
    }
  }, 3000);

  onUnmounted(() => {
    offInitial();
    offOverview();
    offUpsert();
    offRemoved();
    window.clearTimeout(fallbackTimer);
  });
});
</script>

<template>
  <AppLayout>
    <template #title>
      <h1 class="dash-title">{{ $t('index.heading') }}</h1>
      <p class="dash-subtitle">{{ $t('index.subtitle', { count: onlineCount }) }}</p>
    </template>

    <section class="overview" data-test="dashboard-view">
      <NodeMap />
      <OverviewStats />
      <NodeList />
    </section>
  </AppLayout>
</template>

<style scoped>
.overview {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.dash-title {
  margin: 0;
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.dash-subtitle {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 13px;
}
</style>
