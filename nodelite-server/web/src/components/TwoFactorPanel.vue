<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SettingsAuth, TwoFactorSetupResponse } from '@/api';
import { apiClient } from '@/api';
import { ApiAbortError } from '@/api/client';
import { messageFromError } from '@/lib/apiError';
import ReauthFields from './ReauthFields.vue';
import SettingsMessage from './SettingsMessage.vue';

const props = defineProps<{ auth: SettingsAuth }>();
const emit = defineEmits<{ changed: [] }>();

const { t } = useI18n();

const pending = ref<TwoFactorSetupResponse | null>(null);
const busy = ref(false);
const message = reactive<{ state: 'ok' | 'error' | null; text: string }>({ state: null, text: '' });
const form = reactive({ currentPassword: '', code: '' });

const mode = computed<'enabled' | 'idle' | 'pending-setup'>(() => {
  if (props.auth.two_factor_enabled) return 'enabled';
  return pending.value ? 'pending-setup' : 'idle';
});

function resetForm(): void {
  form.currentPassword = '';
  form.code = '';
}

async function startSetup(): Promise<void> {
  busy.value = true;
  message.state = null;
  message.text = t('settings.security.starting_2fa');
  try {
    pending.value = await apiClient.twoFactorStart();
    message.state = null;
    message.text = '';
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('settings.security.action_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    busy.value = false;
  }
}

function cancelSetup(): void {
  pending.value = null;
  resetForm();
  message.state = null;
  message.text = '';
}

async function enable(): Promise<void> {
  busy.value = true;
  message.state = null;
  message.text = t('settings.security.enabling_2fa');
  try {
    await apiClient.twoFactorEnable({
      current_password: form.currentPassword,
      secret: pending.value?.secret ?? '',
      code: form.code,
    });
    pending.value = null;
    resetForm();
    message.state = 'ok';
    message.text = t('settings.security.enabled_saved');
    emit('changed');
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('settings.security.action_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    busy.value = false;
  }
}

async function disable(): Promise<void> {
  busy.value = true;
  message.state = null;
  message.text = t('settings.security.disabling_2fa');
  try {
    await apiClient.twoFactorDisable({
      current_password: form.currentPassword,
      code: form.code,
    });
    resetForm();
    message.state = 'ok';
    message.text = t('settings.security.disabled_saved');
    emit('changed');
  } catch (e) {
    if (e instanceof ApiAbortError) return;
    message.state = 'error';
    message.text = t('settings.security.action_failed', { error: messageFromError(e, 'unknown') });
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <article class="panel" data-test="two-factor-panel">
    <h2 class="card-title">{{ t('settings.security.2fa') }}</h2>

    <!-- Already enabled → disable form -->
    <template v-if="mode === 'enabled'">
      <p class="note">{{ t('settings.security.disable_note') }}</p>
      <form data-test="disable-2fa-form" @submit.prevent="disable">
        <ReauthFields
          v-model:current-password="form.currentPassword"
          v-model:code="form.code"
          variant="both"
        />
        <button type="submit" class="btn btn--danger" :disabled="busy" data-test="disable-2fa">
          {{ t('settings.security.disable_2fa') }}
        </button>
      </form>
    </template>

    <!-- Not enabled, no setup in progress → start -->
    <template v-else-if="mode === 'idle'">
      <p class="note">{{ t('settings.security.2fa_note') }}</p>
      <button type="button" class="btn btn--primary" :disabled="busy" data-test="start-2fa" @click="startSetup">
        {{ t('settings.security.start_2fa') }}
      </button>
    </template>

    <!-- Setup in progress → QR + enable form -->
    <template v-else>
      <p class="note">{{ t('settings.security.setup_note') }}</p>
      <!-- eslint-disable-next-line vue/no-v-html -- qr_svg is a server-generated QR SVG from our own /api/settings/2fa/start, not user input -->
      <div class="qr" data-test="totp-qr" v-html="pending!.qr_svg" />
      <code class="secret" data-test="totp-secret">{{ pending!.secret }}</code>
      <form data-test="enable-2fa-form" @submit.prevent="enable">
        <ReauthFields
          v-model:current-password="form.currentPassword"
          v-model:code="form.code"
          variant="both"
        />
        <div class="actions">
          <button type="submit" class="btn btn--primary" :disabled="busy" data-test="enable-2fa">
            {{ t('settings.security.enable_2fa') }}
          </button>
          <button type="button" class="btn" :disabled="busy" data-test="cancel-2fa" @click="cancelSetup">
            {{ t('settings.security.cancel_setup') }}
          </button>
        </div>
      </form>
    </template>

    <SettingsMessage :state="message.state" :text="message.text" />
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
.note {
  color: var(--text-muted);
  font-size: 12px;
  margin: 0 0 12px;
}
.qr {
  width: 180px;
  height: 180px;
  background: #fff;
  border-radius: 12px;
  padding: 8px;
  margin-bottom: 10px;
}
.qr :deep(svg) {
  width: 100%;
  height: 100%;
  display: block;
}
.secret {
  display: inline-block;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  background: var(--bg-card-soft);
  border-radius: 6px;
  padding: 6px 8px;
  margin-bottom: 12px;
  word-break: break-all;
}
form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.actions {
  display: flex;
  gap: 8px;
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
.btn--danger {
  color: var(--accent-red);
  border-color: var(--accent-red-soft);
  background: var(--accent-red-soft);
}
</style>
