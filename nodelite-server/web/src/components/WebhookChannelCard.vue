<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { WebhookDraft } from '@/lib/alertsDraft';

/**
 * Webhook channel editor. Same secret keep/clear contract as SMTP: the stored
 * secret is never echoed; placeholder when one is on file, type to replace,
 * clear checkbox to wipe.
 */
const webhook = defineModel<WebhookDraft>({ required: true });

const { t } = useI18n();
const summary = computed(() => webhook.value.url || t('settings.disabled'));
</script>

<template>
  <article class="panel" data-test="webhook-card">
    <header class="card-head">
      <h2 class="card-title">{{ t('alerts.webhook.title') }}</h2>
      <label class="toggle">
        <input v-model="webhook.enabled" type="checkbox" data-test="webhook-enabled" />
        <span>{{ t('alerts.webhook.enabled') }}</span>
      </label>
    </header>

    <p v-if="!webhook.enabled" class="collapsed-note" data-test="webhook-collapsed">
      {{ summary }}
    </p>

    <div v-else class="form" data-test="webhook-form">
      <label class="field">
        <span>{{ t('alerts.webhook.url') }}</span>
        <input v-model="webhook.url" type="text" data-test="webhook-url" />
      </label>
      <label class="field">
        <span>{{ t('alerts.webhook.secret') }}</span>
        <input
          v-model="webhook.secret"
          type="password"
          autocomplete="new-password"
          :placeholder="webhook.secret_configured ? t('alerts.secret.keep') : ''"
          :disabled="webhook.clear_secret"
          data-test="webhook-secret"
        />
      </label>
      <label class="toggle">
        <input v-model="webhook.clear_secret" type="checkbox" data-test="webhook-clear-secret" />
        <span>{{ t('alerts.secret.clear') }}</span>
      </label>
      <label class="toggle">
        <input v-model="webhook.send_resolved" type="checkbox" data-test="webhook-send-resolved" />
        <span>{{ t('alerts.webhook.send_resolved') }}</span>
      </label>
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
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
  gap: 12px;
}
.card-title {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
}
.collapsed-note {
  margin: 0;
  background: var(--bg-card-soft);
  border: 1px dashed var(--border-soft);
  border-radius: 10px;
  color: var(--text-muted);
  font-size: 13px;
  padding: 12px;
}
.form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
  color: var(--text-muted);
}
.field input {
  width: 100%;
  background: var(--bg-card-soft);
  color: var(--text-primary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font: inherit;
}
.field input:disabled {
  opacity: 0.5;
}
.toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--text-secondary);
}
@media (max-width: 560px) {
  .card-head {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
