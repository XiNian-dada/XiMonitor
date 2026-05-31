<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SettingsResponse } from '@/api';
import { apiClient } from '@/api';
import { ApiAbortError } from '@/api/client';
import { isNewerVersion, normalizeVersionTag } from '@/lib/version';
import { messageFromError } from '@/lib/apiError';
import ReauthFields from './ReauthFields.vue';
import SettingsMessage from './SettingsMessage.vue';

const props = defineProps<{ settings: SettingsResponse }>();
const { t } = useI18n();

const twoFactor = computed(() => props.settings.auth.two_factor_enabled);

// --- Check for update (direct GitHub call) ---
const checkMsg = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });
const checking = ref(false);

function githubLatestUrl(): string | null {
  const repo = props.settings.repository.replace(/\/+$/, '');
  if (!repo.startsWith('https://github.com/')) return null;
  return `${repo.replace('https://github.com/', 'https://api.github.com/repos/')}/releases/latest`;
}

async function checkForUpdate(): Promise<void> {
  const url = githubLatestUrl();
  if (!url) return;
  checking.value = true;
  checkMsg.state = null;
  checkMsg.text = t('settings.version.checking');
  try {
    const res = await fetch(url, { headers: { accept: 'application/vnd.github+json' } });
    if (!res.ok) throw new Error(`GitHub ${res.status}`);
    const body = (await res.json()) as { tag_name?: string };
    const latest = normalizeVersionTag(body.tag_name ?? '');
    if (latest && isNewerVersion(latest, props.settings.server_version)) {
      checkMsg.state = 'ok';
      checkMsg.text = t('settings.version.update_available', { version: latest });
    } else {
      checkMsg.state = 'ok';
      checkMsg.text = t('settings.version.up_to_date', { version: props.settings.server_version });
    }
  } catch (e) {
    checkMsg.state = 'error';
    checkMsg.text = t('settings.version.check_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    checking.value = false;
  }
}

// --- Manual server update (POST with reauth) ---
const reauth = reactive({ currentPassword: '', code: '' });
const updateMsg = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });
const updating = ref(false);

async function submitUpdate(): Promise<void> {
  updating.value = true;
  updateMsg.state = null;
  updateMsg.text = t('settings.version.update_starting');
  // server-update reauth: 2FA → code only; else current_password only.
  const payload = twoFactor.value
    ? { code: reauth.code }
    : { current_password: reauth.currentPassword };
  try {
    const res = await apiClient.updateServer(payload);
    updateMsg.state = res.ok ? 'ok' : 'error';
    updateMsg.text = res.ok ? t('settings.version.update_started') : res.message;
    if (res.ok) {
      reauth.currentPassword = '';
      reauth.code = '';
    }
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    updateMsg.state = 'error';
    updateMsg.text = t('settings.version.update_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    updating.value = false;
  }
}
</script>

<template>
  <article class="panel" data-test="server-update-card">
    <h2 class="card-title">{{ t('settings.version.title') }}</h2>
    <div class="kv">
      <span class="kv__label">{{ t('settings.version.current') }}</span>
      <span class="kv__value" data-test="server-version">{{ settings.server_version }}</span>
      <span class="kv__label">{{ t('settings.version.repository') }}</span>
      <span class="kv__value">{{ settings.repository }}</span>
      <span class="kv__label">{{ t('settings.version.public_url') }}</span>
      <span class="kv__value">{{ settings.public_base_url }}</span>
      <span class="kv__label">{{ t('settings.version.listen') }}</span>
      <span class="kv__value">{{ settings.listen }}</span>
    </div>

    <div class="actions">
      <button type="button" class="btn" :disabled="checking" data-test="check-update" @click="checkForUpdate">
        {{ t('settings.version.check_updates') }}
      </button>
      <a class="btn btn--link" :href="settings.updates.latest_release_url" target="_blank" rel="noopener">
        {{ t('settings.version.open_release') }}
      </a>
    </div>
    <SettingsMessage :state="checkMsg.state" :text="checkMsg.text" />

    <form class="update-form" data-test="server-update-form" @submit.prevent="submitUpdate">
      <p class="note">
        {{ twoFactor ? t('settings.version.manual_update_note_2fa') : t('settings.version.manual_update_note_password') }}
      </p>
      <ReauthFields
        v-model:current-password="reauth.currentPassword"
        v-model:code="reauth.code"
        :two-factor-enabled="twoFactor"
        variant="server-update"
      />
      <button type="submit" class="btn btn--primary" :disabled="updating" data-test="server-update-submit">
        {{ t('settings.version.update_now') }}
      </button>
      <SettingsMessage :state="updateMsg.state" :text="updateMsg.text" />
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
}
.actions {
  display: flex;
  gap: 8px;
  margin: 14px 0 4px;
}
.note {
  color: var(--text-muted);
  font-size: 12px;
  margin: 14px 0 10px;
}
.update-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
  border-top: 1px solid var(--border-soft);
  margin-top: 14px;
  padding-top: 14px;
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
.btn:hover:not([disabled]) {
  color: var(--text-primary);
}
.btn--link {
  display: inline-flex;
  align-items: center;
  text-decoration: none;
}
.btn--primary {
  color: #fff;
  background: var(--accent-blue);
  border-color: transparent;
}
</style>
