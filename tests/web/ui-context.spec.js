import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function createSecret(page, name) {
  await page.locator('#new-secret').click();
  await page.locator('#secret-form input[name="name"]').fill(name);
  await page.locator('#secret-form textarea[name="value"]').fill('fixture-only');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

async function attemptProgrammaticSwitch(page) {
  await page.locator('#workspace-select').evaluate((selector) => {
    selector.value = 'sandbox';
    selector.dispatchEvent(new Event('change', { bubbles: true }));
  });
}

test('context rail repeats scope and guards a dirty workspace switch', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  const scope = page.locator('#context-line');
  await expect(scope).toContainText('local / playwright');
  await expect(page.locator('#context-rail')).toBeVisible();
  await expect(page.locator('#context-version')).not.toBeEmpty();

  await page.locator('#new-secret').click();
  await expect(page.locator('#drawer-context')).toHaveText(/local \/ playwright/);
  await expect(page.locator('#context-rail')).toHaveAttribute('inert', '');
  await page.locator('#secret-form input[name="name"]').fill('preserved-draft');

  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();

  await expect(page.locator('#secret-form input[name="name"]')).toHaveValue('preserved-draft');
  await expect(page.locator('#workspace-select')).toHaveValue('playwright');
  await expect(scope).toContainText('local / playwright');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('Commands, Help, and Settings are real keyboard-accessible surfaces', async ({ page, baseURL }) => {
  await page.goto(baseURL);

  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  const commands = page.getByRole('dialog', { name: 'Commands' });
  await expect(commands).toBeVisible();
  await expect(commands.getByRole('button', { name: 'Search secrets' })).toBeFocused();
  await commands.getByRole('button', { name: 'Search secrets' }).click();
  await expect(page.locator('#search')).toBeFocused();

  await page.locator('#help-open').click();
  const help = page.getByRole('dialog', { name: 'Help' });
  await expect(help).toContainText(/effective context/i);
  await expect(help).toContainText(/Ctrl\+K|⌘K/);
  await page.keyboard.press('Escape');
  await expect(page.locator('#help-open')).toBeFocused();

  await page.locator('#settings-open').click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByLabel('Theme')).toBeFocused();
  await expect(settings.locator('#settings-error')).toHaveCount(1);
  await settings.getByLabel('Theme').selectOption('dark');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.keyboard.press('Escape');
  await expect(page.locator('#settings-open')).toBeFocused();
});

test('missing-token recovery keeps the brand but hides context controls', async ({ page, baseURL }) => {
  const origin = new URL(baseURL).origin;
  await page.goto(origin);
  await expect(page.locator('#auth-recovery')).toBeVisible();
  await expect(page.locator('#context-rail .brand')).toBeVisible();
  await expect(page.locator('#workspace-select')).toBeHidden();
  await expect(page.locator('#commands-open')).toBeHidden();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('confirmed switching publishes context and initial rows as one snapshot', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  await expect(page.locator('#secret-list-summary')).toContainText('0 secrets');
  await expect(page.locator('#dropzone-context')).toHaveText(/local \/ sandbox/);

  await page.locator('#workspace-select').selectOption('playwright');
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await page.locator('#new-secret').click();
  await expect(page.locator('#drawer-context')).toHaveText(/local \/ playwright/);
  await page.locator('#secret-form input[name="name"]').fill('sandbox-secret');
  await page.locator('#secret-form textarea[name="value"]').fill('fixture-only');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await page.getByRole('button', { name: 'Edit secret sandbox-secret' }).click();
  await page.locator('#delete').click();
  await expect(page.locator('#delete-confirmation-message')).toContainText('local vault playwright');
  await expect(page.locator('#delete-confirmation .confirmation-context')).toHaveText(/local \/ playwright/);
});

test('two tabs retain independent effective contexts and scoped request targets', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  const secondTab = await page.context().newPage();
  await secondTab.goto(baseURL);
  await secondTab.locator('#workspace-select').selectOption('sandbox');
  await expect(secondTab.locator('#context-line')).toContainText('local / sandbox');
  await expect(page.locator('#context-line')).toContainText('local / playwright');

  const scopedPut = page.waitForRequest((request) => (
    request.method() === 'PUT' && request.url().includes('/api/secrets/tab-one')
  ));
  await createSecret(page, 'tab-one');
  const requestUrl = new URL((await scopedPut).url());
  expect(requestUrl.searchParams.get('alias')).toBe('playwright');
  expect(requestUrl.searchParams.get('backend')).toBe('local');
  expect(requestUrl.searchParams.get('vault')).toBe('playwright');
  await expect(secondTab.locator('#context-line')).toContainText('local / sandbox');
  await secondTab.close();
});

test('bulk, Undo, restore, and purge hold one immutable scope and lock switching', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'scope-lock');
  await expect(page.locator('#workspace-select')).toBeEnabled();

  let releaseDelete;
  const deleteGate = new Promise((resolve) => { releaseDelete = resolve; });
  await page.route('**/api/secrets/scope-lock?**', async (route) => {
    if (route.request().method() === 'DELETE') await deleteGate;
    await route.continue();
  });
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret scope-lock' }).check();
  await page.locator('#bulk-delete-secrets').click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await page.getByRole('dialog', { name: 'Delete secret?' })
    .getByRole('button', { name: 'Delete secret' }).click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await expect(page.locator('#progress-context')).toHaveText(/local \/ playwright/);
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#workspace-select')).toHaveValue('playwright');
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  releaseDelete();
  await expect(page.locator('#action-notice')).toContainText('moved to Trash');
  await expect(page.locator('#action-notice-context')).toHaveText(/local \/ playwright/);

  let releaseUndo;
  const undoGate = new Promise((resolve) => { releaseUndo = resolve; });
  await page.route('**/api/secrets/scope-lock/restore?**', async (route) => {
    await undoGate;
    await route.continue();
  });
  await page.locator('#undo-delete').click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#workspace-select')).toHaveValue('playwright');
  releaseUndo();
  await page.locator('#cancel-secret-selection').click();
  await expect(page.getByRole('button', { name: 'Edit secret scope-lock' })).toBeVisible();

  await page.unrouteAll({ behavior: 'wait' });
  await page.getByRole('button', { name: 'Edit secret scope-lock' }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await page.getByRole('dialog', { name: 'Delete secret?' })
    .getByRole('button', { name: 'Delete secret' }).click();
  await page.getByRole('tab', { name: 'Trash' }).click();

  let releaseRestore;
  const restoreGate = new Promise((resolve) => { releaseRestore = resolve; });
  await page.route('**/api/secrets/scope-lock/restore?**', async (route) => {
    await restoreGate;
    await route.continue();
  });
  const trashRow = page.getByRole('row', { name: /scope-lock/ });
  await trashRow.getByRole('button', { name: 'Restore scope-lock' }).click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  releaseRestore();
  await expect(trashRow).toHaveCount(0);

  await page.unrouteAll({ behavior: 'wait' });
  await page.getByRole('tab', { name: 'Secrets' }).click();
  await page.getByRole('button', { name: 'Edit secret scope-lock' }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await page.getByRole('dialog', { name: 'Delete secret?' })
    .getByRole('button', { name: 'Delete secret' }).click();
  await page.getByRole('tab', { name: 'Trash' }).click();

  let releasePurge;
  const purgeGate = new Promise((resolve) => { releasePurge = resolve; });
  await page.route('**/api/secrets/scope-lock/purge?**', async (route) => {
    await purgeGate;
    await route.continue();
  });
  const purgeRow = page.getByRole('row', { name: /scope-lock/ });
  await purgeRow.getByRole('button', { name: 'Purge scope-lock' }).click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  const purgeDialog = page.getByRole('dialog', { name: 'Permanently purge scope-lock?' });
  await purgeDialog.getByLabel('Type scope-lock to confirm').fill('scope-lock');
  await purgeDialog.getByRole('button', { name: 'Permanently purge' }).click();
  await expect(page.locator('#workspace-select')).toBeDisabled();
  await attemptProgrammaticSwitch(page);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  releasePurge();
  await expect(purgeRow).toHaveCount(0);
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('a pending workspace switch blocks reverse-order scoped actions and stale drawers', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'reverse-edit');
  await createSecret(page, 'reverse-trash');
  await page.getByRole('button', { name: 'Edit secret reverse-trash' }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await page.getByRole('dialog', { name: 'Delete secret?' })
    .getByRole('button', { name: 'Delete secret' }).click();
  await page.getByRole('tab', { name: 'Trash' }).click();
  await expect(page.getByRole('button', { name: 'Restore reverse-trash' })).toBeVisible();
  await page.getByRole('tab', { name: 'Secrets' }).click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret reverse-edit' }).check();
  await page.locator('#secret-move-folder').fill('blocked-folder');

  let releaseActivation;
  const activationGate = new Promise((resolve) => { releaseActivation = resolve; });
  await page.route('**/api/workspaces/activate', async (route) => {
    await activationGate;
    await route.continue();
  });
  const scopedRequests = [];
  page.on('request', (request) => {
    if (/\/api\/(?:secrets|files)/.test(request.url())
      && ['POST', 'PUT', 'PATCH', 'DELETE'].includes(request.method())) {
      scopedRequests.push([request.method(), request.url()]);
    }
  });

  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expect(page.locator('#app-header')).toHaveAttribute('inert', '');
  await expect(page.locator('#progress-context')).toHaveText(/local \/ playwright/);

  await page.evaluate(() => {
    document.querySelector('#new-secret').click();
    document.querySelector('[aria-label="Edit secret reverse-edit"]').click();
    document.querySelector('#bulk-delete-secrets').click();
    document.querySelector('#bulk-move-secrets').click();
    document.querySelector('#undo-delete').click();
    document.querySelector('[aria-label="Restore reverse-trash"]').click();
    document.querySelector('[aria-label="Purge reverse-trash"]').click();
    const input = document.querySelector('#file-input');
    const transfer = new DataTransfer();
    transfer.items.add(new File(['blocked'], 'blocked.txt', { type: 'text/plain' }));
    input.files = transfer.files;
    input.dispatchEvent(new Event('change', { bubbles: true }));
  });

  await expect(page.locator('#drawer')).toBeHidden();
  await expect(page.locator('#delete-confirmation')).toBeHidden();
  await expect(page.locator('#purge-confirmation')).toBeHidden();
  expect(scopedRequests).toEqual([]);

  releaseActivation();
  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  await expect(page.locator('#drawer')).toBeHidden();
  expect(scopedRequests).toEqual([]);
});

test('Settings failures remain global after close and explicit Retry recovers safely', async ({ page, baseURL }) => {
  let getAttempts = 0;
  let putAttempts = 0;
  await page.route('**/api/preferences', async (route) => {
    const method = route.request().method();
    if (method === 'GET') {
      getAttempts++;
      if (getAttempts === 1) {
        await route.fulfill({
          status: 500,
          json: { error: { message: 'Settings load failed', hint: 'Retry the read.' } },
        });
        return;
      }
      await route.fulfill({ status: 200, json: { version: 1, theme: 'system' } });
      return;
    }
    putAttempts++;
    if (putAttempts === 1) {
      await route.fulfill({
        status: 500,
        json: { error: { message: 'Settings save failed', hint: 'Retry the write.' } },
      });
      return;
    }
    await route.fulfill({ status: 200, body: route.request().postData() });
  });

  await page.goto(baseURL);
  const status = page.locator('#settings-status');
  await expect(status).toBeVisible();
  await expect(status).toContainText('Settings load failed');
  await expect(page.locator('#settings-open')).toHaveAttribute('data-error', 'true');
  await page.locator('#settings-retry').click();
  await expect(status).toBeHidden();
  await expect(page.locator('#settings-open')).not.toHaveAttribute('data-error', 'true');

  await page.locator('#settings-open').click();
  await page.getByRole('dialog', { name: 'Settings' }).getByLabel('Theme').selectOption('dark');
  await page.keyboard.press('Escape');
  await expect(page.locator('#settings-open')).toBeFocused();
  await expect(status).toBeVisible();
  await expect(status).toContainText('Settings save failed');
  await expect(page.locator('#settings-open')).toHaveAttribute('data-error', 'true');
  await page.locator('#settings-retry').click();
  await expect(status).toBeHidden();

  await createSecret(page, 'settings-retry-vault-usable');
  await expect(page.getByRole('button', { name: 'Edit secret settings-retry-vault-usable' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});
