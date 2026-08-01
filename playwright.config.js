import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/web',
  timeout: 60_000,
  workers: 1,
  outputDir: 'test-results/playwright',
  snapshotPathTemplate: '{testDir}/snapshots/{projectName}/{arg}{ext}',
  use: {
    headless: true,
    reducedMotion: 'reduce',
  },
  projects: [
    {
      name: 'functional-chromium',
      testIgnore: /ui-visual\.spec\.js/,
    },
    ...[
      ['visual-1180x760', 1180, 760],
      ['visual-820x560', 820, 560],
      ['visual-768x700', 768, 700],
      ['visual-390x844', 390, 844],
    ].map(([name, width, height]) => ({
      name,
      testMatch: /ui-visual\.spec\.js/,
      use: {
        viewport: { width, height },
        locale: 'en-US',
        timezoneId: 'UTC',
      },
    })),
  ],
});
