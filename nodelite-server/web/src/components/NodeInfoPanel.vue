<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { NodeStatus } from '@/api';
import { fmtBytes, uptimeParts } from '@/lib/format';
import { totalDiskBytes, uniqueDisks } from '@/lib/disks';

const props = defineProps<{ node: NodeStatus | null }>();

const { t } = useI18n();

function uptimeText(seconds: number | null | undefined): string {
  const parts = uptimeParts(seconds);
  if (!parts) return t('common.not_available');
  const named = { days: parts.days, hours: parts.hours, minutes: parts.minutes };
  if (parts.days > 0) return t('node.uptime.days_hours', named);
  if (parts.hours > 0) return t('node.uptime.hours_minutes', named);
  return t('node.uptime.minutes', named);
}

const rows = computed<Array<{ label: string; value: string }>>(() => {
  const node = props.node;
  if (!node) return [];
  const id = node.identity;
  const snapshot = node.snapshot;
  const disks = uniqueDisks(snapshot?.disks);
  const totalDisk = totalDiskBytes(disks);

  const cpuLine = id.cpu_cores
    ? `${t('node.info.cores', { count: id.cpu_cores })}${id.cpu_model ? ` · ${id.cpu_model}` : ''}`
    : (id.cpu_model ?? t('common.unknown'));

  return [
    { label: t('node.info.os'), value: id.os || t('common.unknown_os') },
    { label: t('node.info.kernel'), value: id.kernel_version || t('common.unknown') },
    { label: t('node.info.cpu'), value: cpuLine },
    {
      label: t('node.info.memory'),
      value: snapshot?.memory.total_bytes
        ? (fmtBytes(snapshot.memory.total_bytes) ?? t('common.not_available'))
        : t('common.not_available'),
    },
    {
      label: t('node.info.disk'),
      value: totalDisk ? (fmtBytes(totalDisk) ?? t('common.not_available')) : t('common.not_available'),
    },
    { label: t('node.info.virtualization'), value: id.agent_version || t('common.unknown') },
    { label: t('node.info.uptime'), value: uptimeText(snapshot?.uptime_secs) },
  ];
});
</script>

<template>
  <article class="panel info-card" data-test="node-info-panel">
    <div class="panel-head">
      <div class="panel-title">{{ t('node.info.title') }}</div>
    </div>
    <div class="info-rows">
      <template v-for="(row, i) in rows" :key="i">
        <div class="label">{{ row.label }}</div>
        <div class="value" data-test="info-value">{{ row.value }}</div>
      </template>
    </div>
  </article>
</template>

<style scoped>
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.panel-head {
  margin-bottom: 12px;
}
.panel-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}
.info-rows {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 8px 16px;
  font-size: 13px;
}
.info-rows .label {
  color: var(--text-muted);
}
.info-rows .value {
  color: var(--text-primary);
  text-align: right;
  word-break: break-word;
}
</style>
