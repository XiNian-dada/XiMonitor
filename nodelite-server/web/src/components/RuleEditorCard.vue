<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type {
  AlertComparator,
  AlertMetric,
  AlertScopeMode,
  AlertSeverity,
} from '@/api';
import type { RuleDraft } from '@/lib/alertsDraft';
import CsvField from './CsvField.vue';
import DeliveryCheckboxes from './DeliveryCheckboxes.vue';

/**
 * Editor for one alert rule, bound directly against the parent's reactive draft
 * slice (single source of truth). The scope selector gates which target field
 * shows (node IDs vs tags). Numeric inputs are integer/step=1/non-negative
 * because the server stores them as u64 — a decimal would 4xx on save.
 * Removal is an array-level concern, so it's emitted to the parent (RuleList).
 */
const rule = defineModel<RuleDraft>({ required: true });
const emit = defineEmits<{ remove: [] }>();

const { t } = useI18n();

const metrics: AlertMetric[] = [
  'cpu_usage_percent',
  'memory_usage_percent',
  'disk_usage_percent',
  'latency_ms',
  'offline_minutes',
];
const comparators: AlertComparator[] = ['gt', 'lt'];
const severities: AlertSeverity[] = ['warning', 'critical'];
const scopes: AlertScopeMode[] = ['all', 'node_ids', 'tags'];

// The i18n keys use short metric aliases, not the full snake_case enum value.
const METRIC_LABEL_KEY: Record<AlertMetric, string> = {
  cpu_usage_percent: 'alerts.metric.cpu',
  memory_usage_percent: 'alerts.metric.memory',
  disk_usage_percent: 'alerts.metric.disk',
  latency_ms: 'alerts.metric.latency',
  offline_minutes: 'alerts.metric.offline',
};

function scopeLabel(): string {
  const r = rule.value;
  if (r.scope_mode === 'node_ids' && r.node_ids.length) return r.node_ids.join(', ');
  if (r.scope_mode === 'tags' && r.tags.length) return r.tags.join(', ');
  return t(`alerts.scope.${r.scope_mode}`);
}

const title = computed(() => rule.value.name || rule.value.id || t('alerts.rules.name'));

const expression = computed(() => {
  const r = rule.value;
  const metric = t(METRIC_LABEL_KEY[r.metric]);
  const comparator = t(`alerts.comparator.${r.comparator}`);
  const delivery = r.delivery.length
    ? r.delivery.map((c) => t(`alerts.channel.${c}`)).join(' + ')
    : t('common.not_available');
  return `${metric} ${comparator} ${r.threshold} · ${r.window_minutes}m · ${scopeLabel()} · ${delivery}`;
});
</script>

<template>
  <section class="rule-card" data-test="rule-card">
    <header class="rule-head">
      <div class="rule-id">
        <strong class="rule-title" data-test="rule-title">{{ title }}</strong>
        <span class="rule-expression" data-test="rule-expression">{{ expression }}</span>
      </div>
      <div class="rule-actions">
        <label class="toggle">
          <input v-model="rule.enabled" type="checkbox" data-test="rule-enabled" />
          <span>{{ t('alerts.rules.enabled') }}</span>
        </label>
        <button type="button" class="btn btn--danger" data-test="rule-remove" @click="emit('remove')">
          {{ t('alerts.rules.remove') }}
        </button>
      </div>
    </header>

    <details class="rule-details">
      <summary>{{ t('alerts.rules.details') }}</summary>
      <div class="grid">
        <label class="field">
          <span>{{ t('alerts.rules.id') }}</span>
          <input v-model="rule.id" type="text" data-test="rule-id" />
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.name') }}</span>
          <input v-model="rule.name" type="text" data-test="rule-name" />
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.metric') }}</span>
          <select v-model="rule.metric" data-test="rule-metric">
            <option v-for="m in metrics" :key="m" :value="m">{{ t(METRIC_LABEL_KEY[m]) }}</option>
          </select>
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.comparator') }}</span>
          <select v-model="rule.comparator" data-test="rule-comparator">
            <option v-for="c in comparators" :key="c" :value="c">{{ t(`alerts.comparator.${c}`) }}</option>
          </select>
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.threshold') }}</span>
          <input v-model.number="rule.threshold" type="number" min="0" step="1" data-test="rule-threshold" />
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.window_minutes') }}</span>
          <input v-model.number="rule.window_minutes" type="number" min="1" step="1" data-test="rule-window" />
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.cooldown_minutes') }}</span>
          <input v-model.number="rule.cooldown_minutes" type="number" min="1" step="1" data-test="rule-cooldown" />
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.severity') }}</span>
          <select v-model="rule.severity" data-test="rule-severity">
            <option v-for="s in severities" :key="s" :value="s">{{ t(`alerts.severity.${s}`) }}</option>
          </select>
        </label>
        <label class="field">
          <span>{{ t('alerts.rules.scope_mode') }}</span>
          <select v-model="rule.scope_mode" data-test="rule-scope">
            <option v-for="s in scopes" :key="s" :value="s">{{ t(`alerts.scope.${s}`) }}</option>
          </select>
        </label>
        <label v-if="rule.scope_mode === 'node_ids'" class="field">
          <span>{{ t('alerts.rules.node_ids') }}</span>
          <CsvField v-model="rule.node_ids" data-test="rule-node-ids" />
        </label>
        <label v-else-if="rule.scope_mode === 'tags'" class="field">
          <span>{{ t('alerts.rules.tags') }}</span>
          <CsvField v-model="rule.tags" data-test="rule-tags" />
        </label>
      </div>

      <div class="field">
        <span>{{ t('alerts.inspection.delivery') }}</span>
        <DeliveryCheckboxes v-model="rule.delivery" data-test="rule-delivery" />
      </div>

      <label class="toggle">
        <input v-model="rule.send_resolved" type="checkbox" data-test="rule-send-resolved" />
        <span>{{ t('alerts.rules.send_resolved') }}</span>
      </label>
    </details>
  </section>
</template>

<style scoped>
.rule-card {
  background: var(--bg-card-soft);
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  padding: 14px 16px;
}
.rule-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}
.rule-id {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.rule-title {
  font-size: 14px;
  color: var(--text-primary);
}
.rule-expression {
  font-size: 12px;
  color: var(--text-muted);
  word-break: break-word;
}
.rule-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}
.rule-details {
  margin-top: 12px;
}
.rule-details summary {
  cursor: pointer;
  font-size: 13px;
  color: var(--text-secondary);
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 12px;
  margin: 12px 0;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
  color: var(--text-muted);
}
.field input,
.field select {
  width: 100%;
  background: var(--bg-card);
  color: var(--text-primary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font: inherit;
}
.toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--text-secondary);
}
.btn {
  background: var(--bg-card);
  color: var(--text-secondary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 6px 12px;
  font: inherit;
}
.btn--danger {
  color: var(--accent-red);
  border-color: var(--accent-red-soft);
  background: var(--accent-red-soft);
}
</style>
