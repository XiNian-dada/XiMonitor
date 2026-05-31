<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

/**
 * Reauth inputs for sensitive settings writes. Which fields show depends on
 * the variant + whether 2FA is enabled (ports the legacy confirmation-field
 * logic in index-settings.js):
 * - 'server-update': 2FA → code only; else current_password only.
 * - 'standard' (default): current_password always; + code when 2FA enabled.
 * - 'both': current_password + code always (e.g. enabling/disabling 2FA).
 */
const props = withDefaults(
  defineProps<{
    // Only consulted by the 'server-update' and 'standard' variants; 'both'
    // always shows password + code, so callers using it can omit this.
    twoFactorEnabled?: boolean;
    variant?: 'server-update' | 'standard' | 'both';
  }>(),
  { twoFactorEnabled: false, variant: 'standard' },
);

const currentPassword = defineModel<string>('currentPassword', { default: '' });
const code = defineModel<string>('code', { default: '' });

const { t } = useI18n();

const showPassword = computed(() =>
  props.variant === 'server-update' ? !props.twoFactorEnabled : true,
);
const showCode = computed(() => {
  if (props.variant === 'both') return true;
  return props.twoFactorEnabled;
});
</script>

<template>
  <div class="reauth-fields" data-test="reauth-fields">
    <label v-if="showPassword" class="reauth-field">
      <span>{{ t('settings.password.current') }}</span>
      <input
        v-model="currentPassword"
        type="password"
        autocomplete="current-password"
        data-test="reauth-password"
        required
      />
    </label>
    <label v-if="showCode" class="reauth-field">
      <span>{{ t('settings.security.verification_code') }}</span>
      <input
        v-model="code"
        type="text"
        inputmode="numeric"
        pattern="[0-9]{6}"
        maxlength="6"
        autocomplete="one-time-code"
        data-test="reauth-code"
        required
      />
    </label>
  </div>
</template>

<style scoped>
.reauth-fields {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.reauth-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
  color: var(--text-muted);
}
.reauth-field input {
  background: var(--bg-card-soft);
  color: var(--text-primary);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font: inherit;
}
</style>
