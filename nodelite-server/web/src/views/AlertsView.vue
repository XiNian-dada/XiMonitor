<script setup lang="ts">
import { onMounted, reactive } from 'vue';
import { useI18n } from 'vue-i18n';
import AppLayout from '@/components/AppLayout.vue';
import AlertOverviewCard from '@/components/AlertOverviewCard.vue';
import SmtpChannelCard from '@/components/SmtpChannelCard.vue';
import WebhookChannelCard from '@/components/WebhookChannelCard.vue';
import InspectionCard from '@/components/InspectionCard.vue';
import RuleList from '@/components/RuleList.vue';
import PreviewCard from '@/components/PreviewCard.vue';
import ReauthFields from '@/components/ReauthFields.vue';
import SettingsMessage from '@/components/SettingsMessage.vue';
import { ApiAbortError } from '@/api/client';
import { messageFromError } from '@/lib/apiError';
import { draftToPayload, emptyAlertsConfig, viewToDraft } from '@/lib/alertsDraft';
import { useAlertsStore } from '@/stores/alerts';

const { t } = useI18n();
const store = useAlertsStore();

// The reactive draft is the single source of truth (no DOM-as-state). Seeded
// from the server config on load and re-seeded after each successful save.
const draft = reactive(emptyAlertsConfig());
// Both reauth fields always show (matches legacy); the server validates whichever
// applies given the account's 2FA state. draftToPayload omits blanks.
const reauth = reactive({ current_password: '', code: '' });
const message = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });

function seedDraft(): void {
  if (store.config) Object.assign(draft, viewToDraft(store.config));
}

onMounted(async () => {
  await store.load();
  seedDraft();
});

async function save(): Promise<void> {
  message.state = null;
  message.text = t('alerts.saving');
  try {
    await store.save(draftToPayload(draft, reauth));
    seedDraft();
    reauth.current_password = '';
    reauth.code = '';
    message.state = 'ok';
    message.text = t('alerts.saved');
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('alerts.save_failed', { error: messageFromError(e, 'unknown') });
  }
}
</script>

<template>
  <AppLayout>
    <template #title>
      <h1 class="page-heading">{{ t('alerts.heading') }}</h1>
      <p class="page-subtitle">{{ t('alerts.subtitle') }}</p>
    </template>

    <section class="alerts" data-test="alerts-view">
      <template v-if="store.config">
        <AlertOverviewCard v-model="draft" />

        <article class="save-bar panel" data-test="alerts-save-bar">
          <ReauthFields
            v-model:current-password="reauth.current_password"
            v-model:code="reauth.code"
            variant="both"
          />
          <div class="save-bar__actions">
            <button
              type="button"
              class="btn btn--primary"
              :disabled="store.saving"
              data-test="alerts-save"
              @click="save"
            >
              {{ t('alerts.save') }}
            </button>
            <SettingsMessage :state="message.state" :text="message.text" />
          </div>
        </article>

        <div class="alerts__grid" :class="{ 'alerts__grid--disabled': !draft.enabled }">
          <SmtpChannelCard v-model="draft.smtp" />
          <WebhookChannelCard v-model="draft.webhook" />
          <InspectionCard v-model="draft.inspection" />
        </div>

        <RuleList v-model="draft.rules" />
        <PreviewCard :preview="store.preview" />
      </template>

      <SettingsMessage
        v-else-if="store.error"
        state="error"
        :text="store.error.message"
        data-test="alerts-error"
      />
      <p v-else class="placeholder" data-test="alerts-loading">
        {{ t('common.waiting_for_data') }}
      </p>
    </section>
  </AppLayout>
</template>

<style scoped>
.alerts {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.alerts__grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
  gap: 16px;
  align-items: start;
}
.alerts__grid--disabled {
  opacity: 0.82;
}
.panel {
  background: var(--bg-card);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 18px 20px;
}
.save-bar {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 16px;
  align-items: end;
}
.save-bar :deep(.reauth-fields) {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
.save-bar__actions {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 10px;
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
.page-heading {
  margin: 0;
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.page-subtitle {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 13px;
}
.placeholder {
  color: var(--text-muted);
  font-size: 13px;
}
@media (max-width: 760px) {
  .save-bar {
    grid-template-columns: 1fr;
  }
  .save-bar :deep(.reauth-fields) {
    grid-template-columns: 1fr;
  }
}
</style>
