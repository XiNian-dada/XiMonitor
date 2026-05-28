import { ref, type Ref } from 'vue';

export type Theme = 'light' | 'dark';

export const THEME_STORAGE_KEY = 'nodelite.ui.theme';

function readStoredTheme(): Theme {
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
    return stored === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

function writeTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // localStorage unavailable (private mode, quota) — DOM still gets the attr
  }
}

let themeRef: Ref<Theme> | null = null;

function ensureRef(): Ref<Theme> {
  if (themeRef === null) {
    themeRef = ref<Theme>(readStoredTheme());
  }
  return themeRef;
}

/**
 * Synchronous bootstrap; must run before the first paint to prevent
 * a light-to-dark flash. The legacy IIFE in assets/index.html:12-20
 * does the same thing; here it's exported so it can be called either
 * from the inline shim in index.html or from main.ts.
 */
export function setupTheme(): Theme {
  const theme = readStoredTheme();
  document.documentElement.dataset.theme = theme;
  ensureRef().value = theme;
  return theme;
}

export function useTheme(): { theme: Ref<Theme>; toggleTheme: () => void } {
  const theme = ensureRef();

  function toggleTheme(): void {
    const next: Theme = theme.value === 'light' ? 'dark' : 'light';
    theme.value = next;
    writeTheme(next);
  }

  return { theme, toggleTheme };
}
