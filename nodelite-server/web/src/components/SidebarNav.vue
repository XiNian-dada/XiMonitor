<script setup lang="ts">
import { useRoute } from 'vue-router';

// Alerts lands later in Stage 2.5; it renders as a disabled icon button so
// the rail matches the legacy layout but doesn't navigate yet.
const route = useRoute();
</script>

<template>
  <aside class="sidebar" aria-label="primary" data-test="sidebar-nav">
    <RouterLink class="brand" to="/" title="NodeLite" aria-label="NodeLite" />

    <RouterLink
      class="nav-button"
      :class="{ active: route.path === '/' }"
      to="/"
      :title="$t('index.nav.overview')"
      :aria-label="$t('index.nav.overview')"
      data-test="nav-overview"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <rect x="3" y="3" width="7" height="7" rx="1.5" />
        <rect x="14" y="3" width="7" height="7" rx="1.5" />
        <rect x="3" y="14" width="7" height="7" rx="1.5" />
        <rect x="14" y="14" width="7" height="7" rx="1.5" />
      </svg>
    </RouterLink>

    <RouterLink
      class="nav-button"
      :class="{ active: route.path === '/settings' }"
      to="/settings"
      :title="$t('index.nav.settings')"
      :aria-label="$t('index.nav.settings')"
      data-test="nav-settings"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <circle cx="12" cy="12" r="3" />
        <path
          d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"
        />
      </svg>
    </RouterLink>

    <button
      type="button"
      class="nav-button"
      disabled
      :title="`${$t('index.nav.alerts')} (Stage 2.5)`"
      :aria-label="$t('index.nav.alerts')"
      data-test="nav-alerts"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <path d="M15 17h5l-1.4-1.4A2 2 0 0 1 18 14.2V11a6 6 0 1 0-12 0v3.2a2 2 0 0 1-.6 1.4L4 17h5" />
        <path d="M9 17a3 3 0 0 0 6 0" />
      </svg>
    </button>

    <div class="sidebar-bottom">
      <RouterLink
        class="nav-button avatar-button"
        :class="{ active: route.path === '/account' }"
        to="/account"
        :title="$t('index.nav.account')"
        :aria-label="$t('index.nav.account')"
        data-test="nav-account"
      >
        <div class="avatar">
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
            <circle cx="12" cy="7" r="4" />
          </svg>
        </div>
      </RouterLink>
    </div>
  </aside>
</template>

<style scoped>
.sidebar {
  background: var(--bg-sidebar);
  border-right: 1px solid var(--border-soft);
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 16px 0 20px;
  gap: 18px;
  position: sticky;
  top: 0;
  height: 100vh;
}
.brand {
  width: 64px;
  height: 64px;
  display: block;
  border-radius: 18px;
  overflow: hidden;
  background-color: transparent;
  background-image: var(--brand-logo);
  background-position: center;
  background-repeat: no-repeat;
  background-size: contain;
  margin-bottom: 8px;
}
.nav-button {
  width: 44px;
  height: 44px;
  display: grid;
  place-items: center;
  border-radius: 12px;
  border: 0;
  background: transparent;
  color: var(--text-muted);
  transition:
    background 150ms ease,
    color 150ms ease,
    transform 150ms ease;
}
.nav-button:hover:not([disabled]) {
  color: var(--text-secondary);
  background: var(--bg-card-soft);
}
.nav-button.active {
  color: var(--accent-blue);
  background: var(--accent-blue-soft);
}
.nav-button[disabled] {
  opacity: 0.4;
  cursor: not-allowed;
}
.nav-button svg {
  width: 20px;
  height: 20px;
}
.sidebar-bottom {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
}
.avatar {
  width: 36px;
  height: 36px;
  border-radius: 50%;
  background: var(--bg-card-soft);
  display: grid;
  place-items: center;
  color: var(--text-muted);
}
.avatar svg {
  width: 18px;
  height: 18px;
}
.avatar-button {
  padding: 0;
}
</style>
