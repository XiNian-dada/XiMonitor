import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { THEME_STORAGE_KEY, setupTheme, useTheme } from './useTheme';

describe('useTheme', () => {
  beforeEach(() => {
    window.localStorage.clear();
    delete document.documentElement.dataset.theme;
  });

  afterEach(() => {
    window.localStorage.clear();
    delete document.documentElement.dataset.theme;
  });

  describe('setupTheme', () => {
    it('honors stored "light"', () => {
      window.localStorage.setItem(THEME_STORAGE_KEY, 'light');
      expect(setupTheme()).toBe('light');
      expect(document.documentElement.dataset.theme).toBe('light');
    });

    it('honors stored "dark"', () => {
      window.localStorage.setItem(THEME_STORAGE_KEY, 'dark');
      expect(setupTheme()).toBe('dark');
      expect(document.documentElement.dataset.theme).toBe('dark');
    });

    it('falls back to dark when no value stored', () => {
      expect(setupTheme()).toBe('dark');
      expect(document.documentElement.dataset.theme).toBe('dark');
    });

    it('falls back to dark for unrecognized values', () => {
      window.localStorage.setItem(THEME_STORAGE_KEY, 'auto');
      expect(setupTheme()).toBe('dark');
      window.localStorage.setItem(THEME_STORAGE_KEY, 'garbage');
      expect(setupTheme()).toBe('dark');
    });
  });

  describe('useTheme', () => {
    it('toggleTheme flips DOM dataset and localStorage', () => {
      window.localStorage.setItem(THEME_STORAGE_KEY, 'dark');
      setupTheme();
      const { theme, toggleTheme } = useTheme();
      expect(theme.value).toBe('dark');

      toggleTheme();
      expect(theme.value).toBe('light');
      expect(document.documentElement.dataset.theme).toBe('light');
      expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('light');

      toggleTheme();
      expect(theme.value).toBe('dark');
      expect(document.documentElement.dataset.theme).toBe('dark');
      expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark');
    });
  });
});
