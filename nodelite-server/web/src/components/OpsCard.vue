<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SettingsResponse } from '@/api';

const props = defineProps<{ settings: SettingsResponse }>();
const { t } = useI18n();

const rows = computed(() => [
  { label: t('settings.ops.config'), value: props.settings.config_path },
  { label: t('settings.ops.registry'), value: props.settings.registry_path },
  { label: t('settings.ops.history'), value: props.settings.history_db_path },
  { label: t('settings.ops.snapshot'), value: props.settings.snapshot_path },
]);
</script>

<template>
  <article class="panel" data-test="ops-card">
    <h2 class="card-title">{{ t('settings.ops.title') }}</h2>
    <div class="kv">
      <template v-for="row in rows" :key="row.label">
        <span class="kv__label">{{ row.label }}</span>
        <span class="kv__value">{{ row.value }}</span>
      </template>
    </div>
    <p class="note">{{ t('settings.ops.server_upgrade') }}</p>
    <pre class="code">{{ settings.updates.server_upgrade_command }}</pre>
    <p class="note">{{ t('settings.ops.agent_upgrade') }}</p>
    <pre class="code">{{ settings.updates.agent_upgrade_command }}</pre>
  </article>
</template>

<style scoped>
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.card-title {
  margin: 0 0 12px;
  font-size: 14px;
  font-weight: 600;
}
.kv {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 6px 16px;
  font-size: 13px;
}
.kv__label {
  color: var(--text-muted);
}
.kv__value {
  color: var(--text-primary);
  text-align: right;
  word-break: break-all;
  font-variant-numeric: tabular-nums;
}
.note {
  color: var(--text-muted);
  font-size: 12px;
  margin: 12px 0 4px;
}
.code {
  margin: 0;
  background: var(--bg-card-soft);
  border-radius: 8px;
  padding: 10px 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  white-space: pre-wrap;
  word-break: break-all;
  color: var(--text-secondary);
}
</style>
