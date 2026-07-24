import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { readFile } from 'node:fs/promises';

const indexPath = new URL('../../desktop/frontend/index.html', import.meta.url);
const cssPath = new URL('../../desktop/frontend/loading.css', import.meta.url);
const scriptPath = new URL('../../desktop/frontend/loading.js', import.meta.url);
const capabilityPath = new URL('../../desktop/src-tauri/capabilities/default.json', import.meta.url);
const buildScriptPath = new URL('../../desktop/src-tauri/build.rs', import.meta.url);

const safeOpenError = {
  code: 'xv-command-failed',
  operation: 'open-config',
  backend: 'unknown',
  vault: '',
  message: 'The configuration file could not be opened.',
  hint: 'Open the file from a terminal.',
  diagnostics: 'The native opener was unavailable.',
};

const mockScript = (snapshot) => `
  window.__mockTauri = {
    snapshot: ${JSON.stringify(snapshot)},
    calls: [],
    listeners: new Map(),
    unlistenCount: 0,
    previewMode: 'auto',
    applyMode: 'auto',
    previewResolvers: [],
    applyResolvers: [],
    openError: null,
  };
  window.__mockTauri.emit = (event, payload) => {
    window.__mockTauri.listeners.get(event)?.({
      event,
      id: 1,
      payload,
    });
  };
  window.__TAURI__ = {
    core: {
      invoke: async (command, args) => {
        const mock = window.__mockTauri;
        mock.calls.push({ command, args });
        if (command === 'startup_status') return structuredClone(mock.snapshot);
        if (command === 'preview_setup') {
          if (mock.previewMode === 'deferred') {
            return new Promise((resolve) => mock.previewResolvers.push(resolve));
          }
          return {
            backend: args.request.backend,
            vault: args.request.vault || args.request.vault_prefix,
          };
        }
        if (command === 'apply_setup') {
          if (mock.applyMode === 'deferred') {
            return new Promise((resolve) => mock.applyResolvers.push(resolve));
          }
          return {
            preview: {
              backend: args.request.backend,
              vault: args.request.vault || args.request.vault_prefix,
            },
            verification: {
              operation: 'list-secrets',
              backend: args.request.backend,
              vault: args.request.vault || args.request.vault_prefix,
            },
          };
        }
        if (command === 'retry_startup') {
          return {
            kind: 'setup-required',
            config_path: '/tmp/isolated-xv.conf',
          };
        }
        if (command === 'open_config' && mock.openError) throw structuredClone(mock.openError);
        return null;
      },
    },
    event: {
      listen: async (event, handler) => {
        window.__mockTauri.listeners.set(event, handler);
        return () => {
          window.__mockTauri.listeners.delete(event);
          window.__mockTauri.unlistenCount += 1;
        };
      },
    },
  };
`;

const boot = async (page, snapshot) => {
  const [index, css, script] = await Promise.all([
    readFile(indexPath, 'utf8'),
    readFile(cssPath, 'utf8'),
    readFile(scriptPath, 'utf8'),
  ]);
  const html = index
    .replace('<link rel="stylesheet" href="loading.css">', `<style>${css}</style>`)
    .replace(
      '<script type="module" src="loading.js"></script>',
      `<script>${mockScript(snapshot)}</script><script type="module">${script.replaceAll('</script', '<\\/script')}</script>`,
    );
  await page.setContent(html);
};

test('mounted setup flow owns stale work and sends only provider allowlisted fields', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await boot(page, {
    kind: 'setup-required',
    config_path: '/Users/example/.config/xv/xv.conf',
  });

  await expect(page.getByRole('heading', { name: /Choose where Crosstache stores secrets/ })).toBeVisible();
  const setupHeading = page.getByRole('heading', { name: /Choose where Crosstache stores secrets/ });
  await expect(setupHeading).toBeFocused();
  await expect(setupHeading).toHaveCSS('outline-color', 'rgb(139, 210, 168)');
  await expect(setupHeading).toHaveCSS('outline-offset', '6px');
  await page.screenshot({
    path: '/tmp/crosstache-task-4-setup-required-390.png',
    fullPage: true,
  });

  await page.getByRole('button', { name: /Connect Azure/ }).click();
  await expect(page.locator('[name="subscription_id"]')).toBeVisible();
  await page.getByRole('button', { name: /Back to providers/ }).click();
  await page.getByRole('button', { name: /Connect AWS/ }).click();
  await expect(page.locator('[name="vault_prefix"]')).toBeVisible();
  await page.getByRole('button', { name: /Back to providers/ }).click();
  await page.getByRole('button', { name: /Create local vault/ }).click();

  await page.getByRole('button', { name: 'Preview setup' }).click();
  await expect(page.locator('[name="store_path"]')).toBeFocused();
  await expect(page.locator('#store_path-error')).toHaveText('Store path is required.');

  await page.locator('[name="store_path"]').fill('/tmp/store');
  await page.locator('[name="key_file"]').fill('/tmp/key');
  await page.locator('[name="vault"]').fill('personal');
  await page.evaluate(() => {
    const form = document.querySelector('form');
    for (const [name, value] of [
      ['client_secret', 'must-not-cross-ipc'],
      ['access_key', 'must-not-cross-ipc'],
    ]) {
      const input = document.createElement('input');
      input.type = 'hidden';
      input.name = name;
      input.value = value;
      form.append(input);
    }
    const unexpected = Object.assign(document.createElement('input'), {
      name: 'unexpected_field',
      value: 'ignored',
    });
    form.append(unexpected);
    unexpected.dispatchEvent(new Event('input', { bubbles: true }));
    unexpected.remove();
    window.__mockTauri.previewMode = 'deferred';
  });
  await page.getByRole('button', { name: 'Preview setup' }).click();
  await expect.poll(() => page.evaluate(() => window.__mockTauri.previewResolvers.length)).toBe(1);
  await page.locator('[name="store_path"]').fill('/tmp/edited-store');
  await page.evaluate(() => {
    window.__mockTauri.previewResolvers.shift()({ backend: 'local', vault: 'personal' });
  });
  await expect(page.getByRole('button', { name: 'Apply setup' })).toBeHidden();
  await expect(page.getByText('Ready to apply')).toHaveCount(0);

  await page.evaluate(() => { window.__mockTauri.previewMode = 'auto'; });
  await page.getByRole('button', { name: 'Preview setup' }).click();
  await expect(page.getByRole('button', { name: 'Apply setup' })).toBeVisible();
  await page.evaluate(() => { window.__mockTauri.applyMode = 'deferred'; });
  await page.getByRole('button', { name: 'Apply setup' }).click();
  await page.locator('[name="vault"]').fill('edited-vault');
  await page.evaluate(() => { window.__mockTauri.applyResolvers.shift()({}); });
  await expect(page.locator('[data-form-status]')).toBeEmpty();
  await expect(page.getByRole('button', { name: 'Apply setup' })).toBeHidden();

  await page.evaluate(() => { window.__mockTauri.previewMode = 'auto'; });
  await page.getByRole('button', { name: 'Preview setup' }).click();
  await expect(page.getByRole('button', { name: 'Apply setup' })).toBeEnabled();

  const previewCalls = await page.evaluate(() =>
    window.__mockTauri.calls.filter(({ command }) => command === 'preview_setup'));
  expect(previewCalls).toHaveLength(3);
  for (const call of previewCalls) {
    expect(call.args.request).not.toHaveProperty('client_secret');
    expect(call.args.request).not.toHaveProperty('access_key');
  }

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
  await page.evaluate(() => window.dispatchEvent(new Event('beforeunload')));
  expect(await page.evaluate(() => window.__mockTauri.unlistenCount)).toBe(1);
});

test('desktop capability grants only the registered startup command surface', async () => {
  const [capability, buildScript] = await Promise.all([
    readFile(capabilityPath, 'utf8').then(JSON.parse),
    readFile(buildScriptPath, 'utf8'),
  ]);
  const commands = [
    'startup_status',
    'preview_setup',
    'apply_setup',
    'retry_startup',
    'open_config',
    'copy_diagnostics',
  ];

  for (const command of commands) {
    expect(buildScript).toContain(`"${command}"`);
    expect(capability.permissions).toContain(`allow-${command.replaceAll('_', '-')}`);
  }
});

test('mounted recovery runs native actions, Retry, CLI disclosure, and cleanup', async ({ page }) => {
  await page.setViewportSize({ width: 1180, height: 800 });
  await boot(page, {
    kind: 'recoverable-failure',
    config_path: '/tmp/isolated-xv.conf',
    error: {
      code: 'xv-auth-failed',
      operation: 'list-secrets',
      backend: 'azure',
      vault: 'team-vault',
      message: 'Authentication with the selected backend failed.',
      hint: "Run 'az login', then try again.",
      diagnostics: 'Azure CLI session is unavailable.',
    },
  });

  await expect(page.getByRole('alert')).toContainText('Authentication');
  const recoveryHeading = page.getByRole('heading', { name: /Crosstache could not open this vault/ });
  await expect(recoveryHeading).toBeFocused();
  await expect(recoveryHeading).toHaveCSS('outline-color', 'rgb(139, 210, 168)');
  await expect(recoveryHeading).toHaveCSS('outline-offset', '6px');
  await page.screenshot({
    path: '/tmp/crosstache-task-4-recovery-desktop.png',
    fullPage: true,
  });

  await page.getByRole('button', { name: 'Show CLI' }).click();
  await expect(page.locator('[data-cli-help]')).toContainText('az login');
  await page.evaluate((error) => { window.__mockTauri.openError = error; }, safeOpenError);
  await page.getByRole('button', { name: 'Open config' }).click();
  await expect(page.locator('[data-action-status]')).toHaveText(safeOpenError.message);
  await page.getByRole('button', { name: 'Copy diagnostics' }).click();
  await expect(page.locator('[data-action-status]')).toHaveText('Safe diagnostics copied.');
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByRole('button', { name: /Create local vault/ })).toBeVisible();

  const actionCalls = await page.evaluate(() =>
    window.__mockTauri.calls.map(({ command }) => command));
  expect(actionCalls).toEqual(expect.arrayContaining([
    'startup_status',
    'open_config',
    'copy_diagnostics',
    'retry_startup',
  ]));
  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(accessibility.violations).toEqual([]);
  await page.evaluate(() => window.dispatchEvent(new Event('beforeunload')));
  expect(await page.evaluate(() => window.__mockTauri.unlistenCount)).toBe(1);
});
