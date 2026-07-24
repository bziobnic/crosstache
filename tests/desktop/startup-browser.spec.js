import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { readFile } from 'node:fs/promises';

import { renderRecovery, renderStartupState } from '../../desktop/frontend/loading.js';

const cssPath = new URL('../../desktop/frontend/loading.css', import.meta.url);

const shell = (view, css) => `<!doctype html>
<html lang="en"><head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Crosstache startup test</title>
  <style>${css}</style>
</head><body>
  <main class="shell">
    <aside class="context" aria-label="Crosstache">
      <div class="brand" aria-label="Crosstache Vault">xv</div>
      <div class="context-copy">
        <p class="context-kicker">Crosstache Vault</p>
        <p>One deliberate path from configuration to a verified vault.</p>
      </div>
      <div class="vault-line" aria-hidden="true"><span></span></div>
      <p class="privacy-note">Credentials stay with your provider tools.</p>
    </aside>
    <article class="task-surface" id="app" aria-live="polite">${view.html}</article>
  </main>
</body></html>`;

const mount = async (page, view) => {
  const css = await readFile(cssPath, 'utf8');
  await page.setContent(shell(view, css));
};

test('setup required is accessible and remains usable at 390px', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await mount(page, renderStartupState({
    kind: 'setup-required',
    config_path: '/Users/example/.config/xv/xv.conf',
  }));

  await expect(page.getByRole('heading', { name: /Choose where Crosstache stores secrets/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Create local vault/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Advanced configuration/ })).toBeVisible();
  await expect(page.locator('.shell')).toHaveCSS('grid-template-columns', '390px');
  await expect(page.locator('.vault-line')).toBeHidden();

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
  await page.screenshot({
    path: '/tmp/crosstache-task-4-setup-required-390.png',
    fullPage: true,
  });
});

test('recovery exposes safe evidence and actions at desktop width', async ({ page }) => {
  await page.setViewportSize({ width: 1180, height: 800 });
  await mount(page, renderRecovery({
    code: 'xv-auth-failed',
    operation: 'list-secrets',
    backend: 'azure',
    vault: 'team-vault',
    message: 'Authentication with the selected backend failed.',
    hint: "Run 'az login', then try again.",
    diagnostics: 'Azure CLI session is unavailable.',
  }));

  await expect(page.getByRole('alert')).toContainText('Authentication');
  await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Copy diagnostics' })).toBeVisible();
  await expect(page.getByText('xv-auth-failed')).toBeVisible();

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
  await page.screenshot({
    path: '/tmp/crosstache-task-4-recovery-desktop.png',
    fullPage: true,
  });
});
