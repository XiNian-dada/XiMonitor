<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { AlertPreview, InspectionPreview } from '@/api';

/**
 * Read-only render of the server-computed alert preview (triggered rules +
 * daily-inspection summary). The server recomputes it on every GET/save, so
 * there's no client logic here — the card just reflects `store.preview`. Null
 * before the first successful load → the "save once to preview" empty state.
 */
const props = defineProps<{ preview: AlertPreview | null }>();

const { t } = useI18n();

type CountField = Exclude<keyof InspectionPreview, 'highlights'>;
const summaryFields: { key: string; field: CountField }[] = [
  { key: 'alerts.preview.total_nodes', field: 'total_nodes' },
  { key: 'alerts.preview.offline_nodes', field: 'offline_nodes' },
  { key: 'alerts.preview.latency_nodes', field: 'latency_nodes' },
  { key: 'alerts.preview.cpu_hot_nodes', field: 'cpu_hot_nodes' },
  { key: 'alerts.preview.memory_hot_nodes', field: 'memory_hot_nodes' },
];

const summary = computed(() => {
  const ins = props.preview?.inspection;
  if (!ins) return [];
  return summaryFields.map(({ key, field }) => ({ key, label: t(key), value: ins[field] }));
});

const triggered = computed(() => props.preview?.triggered_rules ?? []);
const highlights = computed(() => props.preview?.inspection.highlights ?? []);
</script>

<template>
  <article class="panel preview" data-test="preview-card">
    <header class="card-head">
      <h2 class="card-title">{{ t('alerts.preview.title') }}</h2>
    </header>

    <p v-if="!preview" class="preview-muted" data-test="preview-empty">
      {{ t('alerts.preview.empty') }}
    </p>

    <template v-else>
      <div class="summary" data-test="preview-summary">
        <div v-for="row in summary" :key="row.key" class="summary-cell">
          <span class="summary-value">{{ row.value }}</span>
          <span class="summary-label">{{ row.label }}</span>
        </div>
      </div>

      <section class="preview-section">
        <h3 class="preview-subtitle">{{ t('alerts.preview.triggered_rules') }}</h3>
        <p v-if="!triggered.length" class="preview-muted" data-test="preview-no-triggered">
          {{ t('alerts.preview.no_triggered_rules') }}
        </p>
        <ul v-else class="preview-list" data-test="preview-triggered">
          <li v-for="rule in triggered" :key="rule.rule_id" class="preview-item">
            <span class="badge" :class="`badge--${rule.severity}`">
              {{ t(`alerts.severity.${rule.severity}`) }}
            </span>
            <span class="preview-name">{{ rule.rule_name }}</span>
            <span class="preview-nodes">{{ rule.node_ids.join(', ') }}</span>
          </li>
        </ul>
      </section>

      <section class="preview-section">
        <h3 class="preview-subtitle">{{ t('alerts.preview.highlights') }}</h3>
        <p v-if="!highlights.length" class="preview-muted" data-test="preview-no-highlights">
          {{ t('alerts.preview.no_highlights') }}
        </p>
        <ul v-else class="preview-list" data-test="preview-highlights">
          <li v-for="h in highlights" :key="h.node_id" class="preview-item">
            <span class="preview-name">{{ h.node_label || h.node_id }}</span>
            <span class="preview-nodes">{{ h.reasons.join(', ') }}</span>
          </li>
        </ul>
      </section>
    </template>
  </article>
</template>

<style scoped>
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.card-title {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(96px, 1fr));
  gap: 10px;
  margin: 14px 0;
}
.summary-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 12px;
  background: var(--bg-card-soft);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
}
.summary-value {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-primary);
}
.summary-label {
  font-size: 12px;
  color: var(--text-muted);
}
.preview-section {
  margin-top: 14px;
}
.preview-subtitle {
  margin: 0 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}
.preview-muted {
  margin: 0;
  font-size: 13px;
  color: var(--text-muted);
}
.preview-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.preview-item {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  font-size: 13px;
}
.preview-name {
  color: var(--text-primary);
  font-weight: 500;
}
.preview-nodes {
  color: var(--text-muted);
  font-size: 12px;
}
.badge {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 999px;
  border: 1px solid var(--border-soft);
  color: var(--text-secondary);
}
.badge--warning {
  color: var(--accent-amber, #d29922);
  border-color: var(--accent-amber, #d29922);
}
.badge--critical {
  color: var(--accent-red);
  border-color: var(--accent-red-soft);
  background: var(--accent-red-soft);
}
</style>
