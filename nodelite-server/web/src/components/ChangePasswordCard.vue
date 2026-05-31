<script setup lang="ts">
import { reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { apiClient } from '@/api';
import { ApiAbortError } from '@/api/client';
import { AUTH_TIMESTAMP_KEY } from '@/auth/expiry';
import { messageFromError } from '@/lib/apiError';
import { generatePassword } from '@/lib/password';
import SettingsMessage from './SettingsMessage.vue';

const { t } = useI18n();

const LOGOUT_DELAY_MS = 900;

const form = reactive({ current_password: '', new_password: '' });
const message = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });
const busy = ref(false);
// Reveal the new-password field after generating so the user can read/copy
// the value before the post-submit redirect to /logout-and-reauth.
const revealNew = ref(false);
// On success the page navigates away after a short delay; keep the submit
// disabled through that window so a double-click can't fire a stale retry.
const leaving = ref(false);

function suggest(): void {
  form.new_password = generatePassword();
  revealNew.value = true;
}

/** Drop the client session and bounce to reauth (same as logout). */
function finishLogout(): void {
  try {
    window.localStorage.removeItem(AUTH_TIMESTAMP_KEY);
  } catch {
    /* localStorage unavailable */
  }
  window.location.assign('/logout-and-reauth');
}

async function submit(): Promise<void> {
  busy.value = true;
  message.state = null;
  message.text = t('settings.password.saving');
  try {
    await apiClient.changePassword({
      current_password: form.current_password,
      new_password: form.new_password,
    });
    message.state = 'ok';
    message.text = t('settings.password.saved');
    // Password changed → session is invalidated; show the message briefly,
    // then drop to reauth (matches legacy 900ms behavior).
    leaving.value = true;
    window.setTimeout(finishLogout, LOGOUT_DELAY_MS);
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('settings.password.failed', { error: messageFromError(e, 'unknown') });
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <article class="panel" data-test="change-password-card">
    <h2 class="card-title">{{ t('settings.password.title') }}</h2>
    <form class="form" data-test="password-form" @submit.prevent="submit">
      <label class="field">
        <span>{{ t('settings.password.current') }}</span>
        <input
          v-model="form.current_password"
          type="password"
          autocomplete="current-password"
          data-test="password-current"
          required
        />
      </label>
      <label class="field">
        <span>{{ t('settings.password.new') }}</span>
        <input
          v-model="form.new_password"
          :type="revealNew ? 'text' : 'password'"
          autocomplete="new-password"
          minlength="8"
          data-test="password-new"
          required
        />
      </label>
      <div class="actions">
        <button type="button" class="btn" data-test="password-generate" @click="suggest">
          {{ t('settings.password.generate') }}
        </button>
        <button type="submit" class="btn btn--primary" :disabled="busy || leaving" data-test="password-submit">
          {{ t('settings.password.submit') }}
        </button>
      </div>
      <SettingsMessage :state="message.state" :text="message.text" />
    </form>
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
  background: var(--bg-card-soft);
  color: var(--text-primary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font: inherit;
}
.actions {
  display: flex;
  gap: 8px;
}
.btn {
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
</style>
