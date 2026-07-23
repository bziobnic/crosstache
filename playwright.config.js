import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/web',
  timeout: 60_000,
  workers: 1,
  outputDir: 'test-results/playwright',
  use: { headless: true },
});
