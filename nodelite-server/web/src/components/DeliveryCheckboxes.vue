<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import type { AlertChannel } from '@/api';

/**
 * Checkbox group that v-models an AlertChannel[] (smtp / webhook). Toggling
 * rebuilds the array in canonical channel order so the saved payload is stable
 * regardless of click order.
 */
const model = defineModel<AlertChannel[]>({ default: () => [] });

const { t } = useI18n();

const channels: AlertChannel[] = ['smtp', 'webhook'];

function isOn(channel: AlertChannel): boolean {
  return model.value.includes(channel);
}

function toggle(channel: AlertChannel, checked: boolean): void {
  const set = new Set(model.value);
  if (checked) set.add(channel);
  else set.delete(channel);
  model.value = channels.filter((c) => set.has(c));
}
</script>

<template>
  <div class="delivery-checkboxes" data-test="delivery-checkboxes">
    <label v-for="channel in channels" :key="channel" class="delivery-option">
      <input
        type="checkbox"
        :checked="isOn(channel)"
        :data-test="`delivery-${channel}`"
        @change="toggle(channel, ($event.target as HTMLInputElement).checked)"
      />
      <span>{{ t(`alerts.channel.${channel}`) }}</span>
    </label>
  </div>
</template>

<style scoped>
.delivery-checkboxes {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
}
.delivery-option {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--text-secondary);
}
</style>
