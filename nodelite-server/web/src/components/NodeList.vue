<script setup lang="ts">
import { useNodesStore } from '@/stores/nodes';
import NodeCard from './NodeCard.vue';

const nodesStore = useNodesStore();
</script>

<template>
  <section class="nodes-section" data-test="node-list">
    <div v-if="nodesStore.nodes.length > 0" class="node-grid">
      <NodeCard
        v-for="node in nodesStore.nodes"
        :key="node.identity.node_id"
        :node="node"
      />
    </div>
    <p v-else class="nodes-empty" data-test="node-list-empty">
      {{ $t('common.waiting_for_data') }}
    </p>
  </section>
</template>

<style scoped>
.nodes-section {
  margin-top: 12px;
}
.node-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 14px;
}
.nodes-empty {
  color: var(--text-muted);
  font-size: 13px;
  margin: 0;
  padding: 24px 0;
}
</style>
