<script setup lang="ts">
import { computed, onMounted } from 'vue';
import SidebarNav from '@/components/SidebarNav.vue';
import OverviewStats from '@/components/OverviewStats.vue';
import NodeMap from '@/components/NodeMap.vue';
import NodeList from '@/components/NodeList.vue';
import { useTheme } from '@/composables/useTheme';
import { usePolling } from '@/composables/usePolling';
import { useLanguage } from '@/i18n/language';
import { SUPPORTED_LOCALES, type SupportedLocale } from '@/i18n';
import { useBootstrapStore } from '@/stores/bootstrap';
import { useOverviewStore } from '@/stores/overview';
import { useNodesStore } from '@/stores/nodes';

const { theme, toggleTheme } = useTheme();
const { currentLocale, setLocale } = useLanguage();

const bootstrapStore = useBootstrapStore();
const overviewStore = useOverviewStore();
const nodesStore = useNodesStore();

const onlineCount = computed(() => overviewStore.data?.online_nodes ?? 0);

onMounted(() => {
  void bootstrapStore.load();
});

// One timer drives both refreshes, matching the legacy single refresh()
// loop. Fixed at the legacy default (5s) for now — bootstrap resolves
// after setup runs, so honoring a server-configured refresh_interval_secs
// would need a restart-on-change and is deferred to a follow-up.
const DEFAULT_REFRESH_MS = 5000;
usePolling(() => {
  void overviewStore.refresh();
  void nodesStore.refresh();
}, DEFAULT_REFRESH_MS);

function onLanguageChange(event: Event): void {
  setLocale((event.target as HTMLSelectElement).value);
}

const localeLabels: Record<SupportedLocale, string> = {
  en: 'English',
  'zh-CN': '中文',
};
</script>

<template>
  <div class="app" data-test="app-shell">
    <SidebarNav />

    <main class="main" data-test="dashboard-view">
      <header class="page-header">
        <div class="page-title">
          <h1>{{ $t('index.heading') }}</h1>
          <p>{{ $t('index.subtitle', { count: onlineCount }) }}</p>
        </div>
        <div class="page-actions">
          <select
            class="lang-select"
            :aria-label="$t('common.language')"
            data-test="language-select"
            :value="currentLocale"
            @change="onLanguageChange"
          >
            <option v-for="locale in SUPPORTED_LOCALES" :key="locale" :value="locale">
              {{ localeLabels[locale] }}
            </option>
          </select>
          <button
            type="button"
            class="theme-toggle"
            :title="$t('common.theme_toggle')"
            :aria-label="$t('common.theme_toggle')"
            data-test="theme-toggle"
            @click="toggleTheme"
          >
            <svg
              v-if="theme === 'dark'"
              class="sun"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <circle cx="12" cy="12" r="4" />
              <path
                d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"
              />
            </svg>
            <svg
              v-else
              class="moon"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
            </svg>
          </button>
        </div>
      </header>

      <section class="overview">
        <NodeMap />
        <OverviewStats />
        <NodeList />
      </section>
    </main>
  </div>
</template>

<style scoped>
.app {
  display: grid;
  grid-template-columns: 72px minmax(0, 1fr);
  min-height: 100vh;
  background: var(--bg-app);
  color: var(--text-primary);
}
.main {
  padding: 24px clamp(20px, 3vw, 36px) 40px;
  max-width: 1680px;
  width: 100%;
}
.page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 24px;
  margin-bottom: 22px;
}
.page-title h1 {
  margin: 0;
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.01em;
}
.page-title p {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 13px;
}
.overview {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.page-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}
.lang-select {
  background: var(--bg-card);
  color: var(--text-secondary);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  padding: 6px 10px;
  font-size: 12px;
}
.theme-toggle {
  width: 36px;
  height: 36px;
  border-radius: 12px;
  border: 1px solid var(--border-soft);
  background: var(--bg-card);
  color: var(--text-secondary);
  display: grid;
  place-items: center;
  transition:
    background 150ms ease,
    color 150ms ease;
}
.theme-toggle:hover {
  color: var(--text-primary);
  background: var(--bg-card-soft);
}
.theme-toggle svg {
  width: 18px;
  height: 18px;
}
</style>
