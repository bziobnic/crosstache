import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/web',
  timeout: 60_000,
  use: { headless: true },
});
