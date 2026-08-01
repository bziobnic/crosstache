import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: /startup-browser\.spec\.js/,
  timeout: 30_000,
  workers: 1,
  outputDir: '../../test-results/desktop-playwright',
  use: {
    headless: true,
    reducedMotion: 'reduce',
  },
});
