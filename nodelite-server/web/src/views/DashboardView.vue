<script setup lang="ts">
import { computed, onMounted } from 'vue';
import AppLayout from '@/components/AppLayout.vue';
import OverviewStats from '@/components/OverviewStats.vue';
import NodeMap from '@/components/NodeMap.vue';
import NodeList from '@/components/NodeList.vue';
import { usePolling } from '@/composables/usePolling';
import { useBootstrapStore } from '@/stores/bootstrap';
import { useOverviewStore } from '@/stores/overview';
import { useNodesStore } from '@/stores/nodes';

const bootstrapStore = useBootstrapStore();
const overviewStore = useOverviewStore();
const nodesStore = useNodesStore();

const onlineCount = computed(() => overviewStore.data?.online_nodes ?? 0);

onMounted(() => {
  void bootstrapStore.load();
});

// One timer drives both refreshes, matching the legacy single refresh()
// loop. Fixed at the legacy default (5s) for now — bootstrap resolves
// after setup runs, so honoring a server-configured refresh_interval_secs
// would need a restart-on-change and is deferred to a follow-up.
const DEFAULT_REFRESH_MS = 5000;
usePolling(() => {
  void overviewStore.refresh();
  void nodesStore.refresh();
}, DEFAULT_REFRESH_MS);
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
