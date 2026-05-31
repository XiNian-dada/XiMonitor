<script setup lang="ts">
import { computed, reactive } from 'vue';
import { useI18n } from 'vue-i18n';
import ReauthFields from '@/components/ReauthFields.vue';
import SettingsMessage from '@/components/SettingsMessage.vue';
import { apiClient } from '@/api';
import { ApiAbortError } from '@/api/client';
import { messageFromError } from '@/lib/apiError';
import { useSettingsStore } from '@/stores/settings';
import { fmtDateTime } from '@/lib/format';

/**
 * Per-node settings tab: shows the current node's token info (from the global
 * settings store's agents array) and a refresh-token form with reauth. The
 * server's POST /api/nodes/{id}/refresh-token returns the new expiry; on
 * success, reload the settings store so the token table reflects the change.
 */
const props = defineProps<{ nodeId: string }>();

const { t } = useI18n();
const settingsStore = useSettingsStore();

const reauth = reactive({ current_password: '', code: '' });
const message = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });
const saving = reactive({ value: false });

const agent = computed(() =>
  settingsStore.data?.agents.find((a) => a.node_id === props.nodeId),
);

const expiryLabel = computed(() => {
  const a = agent.value;
  if (!a) return '—';
  if (!a.token_expires_at) return t('node.settings.token_never_expires');
  const secs = a.token_expires_in_secs;
  if (secs == null || secs < 0) return t('node.settings.token_expired');
  const days = Math.floor(secs / 86400);
  if (days > 0) return t('node.settings.token_expires_in_days', { days });
  const hours = Math.floor(secs / 3600);
  return t('node.settings.token_expires_in_hours', { hours });
});

const expiryDate = computed(() => {
  const a = agent.value;
  return a?.token_expires_at ? fmtDateTime(a.token_expires_at) : null;
});

async function refresh(): Promise<void> {
  message.state = null;
  message.text = t('node.settings.refreshing');
  saving.value = true;
  try {
    const body: { current_password?: string; code?: string } = {};
    if (reauth.current_password) body.current_password = reauth.current_password;
    if (reauth.code) body.code = reauth.code;
    const resp = await apiClient.refreshNodeToken(props.nodeId, body);
    await settingsStore.refresh();
    reauth.current_password = '';
    reauth.code = '';
    message.state = 'ok';
    message.text = resp.message || t('node.settings.token_refreshed');
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('node.settings.refresh_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="node-settings" data-test="node-settings-panel">
    <article class="panel">
      <header class="card-head">
        <h2 class="card-title">{{ t('node.settings.token_info') }}</h2>
      </header>

      <template v-if="agent">
        <div class="info-grid">
          <div class="info-row">
            <span class="info-label">{{ t('node.settings.token_status') }}</span>
            <span class="info-value">{{ expiryLabel }}</span>
          </div>
          <div v-if="expiryDate" class="info-row">
            <span class="info-label">{{ t('node.settings.token_expires_at') }}</span>
            <span class="info-value">{{ expiryDate }}</span>
          </div>
        </div>
      </template>
      <p v-else class="placeholder">
        {{ t('common.waiting_for_data') }}
      </p>
    </article>

    <article class="panel">
      <header class="card-head">
        <h2 class="card-title">{{ t('node.settings.refresh_token') }}</h2>
        <p class="card-note">{{ t('node.settings.refresh_note') }}</p>
      </header>

      <div class="refresh-form">
        <ReauthFields
          v-model:current-password="reauth.current_password"
          v-model:code="reauth.code"
          variant="both"
        />
        <button
          type="button"
          class="btn btn--primary"
          :disabled="saving.value"
          data-test="refresh-token-button"
          @click="refresh"
        >
          {{ t('node.settings.refresh_button') }}
        </button>
        <SettingsMessage :state="message.state" :text="message.text" />
      </div>
    </article>
  </div>
</template>

<style scoped>
.node-settings {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.card-head {
  margin-bottom: 14px;
}
.card-title {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
}
.card-note {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 12px;
}
.info-grid {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.info-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 12px;
  background: var(--bg-card-soft);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
}
.info-label {
  font-size: 13px;
  color: var(--text-muted);
}
.info-value {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
}
.placeholder {
  margin: 0;
  color: var(--text-muted);
  font-size: 13px;
}
.refresh-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.btn {
  align-self: flex-start;
  background: var(--bg-card-soft);
  color: var(--text-secondary);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  padding: 8px 14px;
  font: inherit;
}
.btn--primary {
  color: #fff;
  background: var(--accent-blue);
  border-color: transparent;
}
.btn:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}
</style>
