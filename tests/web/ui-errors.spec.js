import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function createSecret(page, name, value = 'must-never-appear-in-diagnostics') {
  await page.locator('#new-secret').click();
  await page.locator('#secret-form input[name="name"]').fill(name);
  await page.locator('#secret-form textarea[name="value"]').fill(value);
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

test('failed refresh keeps the last snapshot stale and recoverable', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'stale-snapshot');

  let failRefresh = true;
  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() === 'GET' && failRefresh) {
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: {
          error: {
            code: 'xv-backend-unavailable',
            message: 'The vault could not be refreshed.',
            hint: 'Check the connection and retry.',
          },
        },
      });
      return;
    }
    await route.continue();
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();

  await expect(page.getByRole('button', { name: 'Edit secret stale-snapshot' })).toBeVisible();
  await expect(page.locator('#secret-refresh-error')).toContainText('Stale');
  await expect(page.locator('#secret-refresh-error').getByRole('button', { name: 'Retry' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);

  failRefresh = false;
  await page.locator('#secret-refresh-error').getByRole('button', { name: 'Retry' }).click();
  await expect(page.locator('#secret-refresh-error')).toBeHidden();
  await expect(page.getByRole('button', { name: 'Edit secret stale-snapshot' })).toBeVisible();
});

test('partial bulk results persist with safe details and retry failed', async ({ page, baseURL }) => {
  await page.addInitScript(() => {
    let copied = '';
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: async (value) => { copied = value; },
        readText: async () => copied,
      },
    });
  });
  await page.goto(baseURL);
  await createSecret(page, 'bulk-ok', 'ok-secret-marker');
  await createSecret(page, 'bulk-failed', 'failed-secret-marker');

  let failTarget = true;
  await page.route('**/api/secrets/bulk-failed?**', async (route) => {
    if (route.request().method() === 'DELETE' && failTarget) {
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        json: {
          error: {
            code: 'xv-conflict',
            message: 'The item changed before deletion.',
            hint: 'Refresh and retry this item.',
            details: {
              value: 'failed-secret-marker',
              note: 'private note marker',
              headers: { Authorization: 'Bearer private-auth-marker' },
            },
          },
        },
      });
      return;
    }
    await route.continue();
  });

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret bulk-ok' }).check();
  await page.getByRole('checkbox', { name: 'Select secret bulk-failed' }).check();
  await page.locator('#bulk-delete-secrets').click();
  await page.getByRole('dialog', { name: 'Delete secrets?' })
    .getByRole('button', { name: 'Cancel' }).click();
  const cancelledStates = await page.evaluate(() => Object.values(
    globalThis.__xvTestStoreSnapshot().operations || {},
  ));
  expect(cancelledStates.some(({ status }) => status === 'cancelled')).toBe(true);
  await page.locator('#bulk-delete-secrets').click();
  await page.getByRole('dialog', { name: 'Delete secrets?' })
    .getByRole('button', { name: 'Delete 2 secrets' }).click();

  const result = page.locator('#secret-error');
  await expect(result).toContainText('1 failed');
  await expect(result.getByRole('button', { name: 'Retry failed' })).toBeVisible();
  await expect(result.getByRole('button', { name: 'Copy details' })).toBeVisible();
  await expect(result).toContainText('bulk-failed');
  await expect(result).not.toContainText(/failed-secret-marker|private note marker|private-auth-marker/);
  await result.getByRole('button', { name: 'Copy details' }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain('code: xv-conflict');
  expect(copied).toContain('backend: local');
  expect(copied).toContain('vault: playwright');
  expect(copied).toContain('failed names: bulk-failed');
  expect(copied).not.toMatch(/failed-secret-marker|private note marker|private-auth-marker/);
  const operationStates = await page.evaluate(() => Object.values(
    globalThis.__xvTestStoreSnapshot().operations || {},
  ));
  expect(operationStates.some(({ status }) => status === 'partially-succeeded')).toBe(true);
  await expectNoSeriousOrCriticalAxeViolations(page);

  failTarget = false;
  await result.getByRole('button', { name: 'Retry failed' }).click();
  await expect(result).toBeHidden();
});

test('stale refresh and partial action failures coexist without handler clobbering', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'coexist-ok');
  await createSecret(page, 'coexist-failed');

  let releaseDelete;
  const deleteGate = new Promise((resolve) => { releaseDelete = resolve; });
  await page.route('**/api/secrets/coexist-failed?**', async (route) => {
    if (route.request().method() === 'DELETE') {
      await deleteGate;
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        json: { error: { code: 'xv-conflict', message: 'Delete conflict.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret coexist-ok' }).check();
  await page.getByRole('checkbox', { name: 'Select secret coexist-failed' }).check();
  await page.locator('#bulk-delete-secrets').click();
  await page.locator('#confirm-delete').click();

  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Refresh failed.', hint: 'Retry refresh.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.getByRole('button', { name: 'Refresh secrets' }).click();

  const refresh = page.locator('#secret-refresh-error');
  await expect(refresh).toContainText('Stale');
  releaseDelete();
  const action = page.locator('#secret-error');
  await expect(action.getByRole('button', { name: 'Retry failed' })).toBeVisible();
  await expect(refresh.getByRole('button', { name: 'Retry' })).toBeVisible();
  await expect(action.getByRole('button', { name: 'Retry failed' })).toBeVisible();
  await expect(action.getByRole('button', { name: 'Copy details' })).toBeVisible();

  await refresh.getByRole('button', { name: 'Dismiss error' }).click();
  await expect(refresh).toBeHidden();
  await expect(action).toBeVisible();
  await expect(action.getByRole('button', { name: 'Retry failed' })).toBeVisible();
});

test('partial action arriving after stale refresh preserves both independent owners', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'reverse-ok');
  await createSecret(page, 'reverse-failed');

  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Still offline.', hint: 'Retry refresh.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/secrets/reverse-failed?**', async (route) => {
    if (route.request().method() === 'DELETE') {
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        json: { error: { code: 'xv-conflict', message: 'Delete conflict.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();
  const refresh = page.locator('#secret-refresh-error');
  await expect(refresh).toContainText('Stale');

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret reverse-ok' }).check();
  await page.getByRole('checkbox', { name: 'Select secret reverse-failed' }).check();
  await page.locator('#bulk-delete-secrets').click();
  await page.locator('#confirm-delete').click();

  const action = page.locator('#secret-error');
  await expect(refresh.getByRole('button', { name: 'Retry' })).toBeVisible();
  await expect(action.getByRole('button', { name: 'Retry failed' })).toBeVisible();
  await expect(action.getByRole('button', { name: 'Copy details' })).toBeVisible();

  await action.getByRole('button', { name: 'Dismiss error' }).click();
  await expect(action).toBeHidden();
  await expect(refresh).toBeVisible();
  await expect(refresh.getByRole('button', { name: 'Retry' })).toBeVisible();
});

test('dismissal releases partial ownership and late retry cannot republish', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'late-ok');
  await createSecret(page, 'late-failed');

  let retrying = false;
  let releaseRetry;
  const retryGate = new Promise((resolve) => { releaseRetry = resolve; });
  await page.route('**/api/secrets/late-failed?**', async (route) => {
    if (route.request().method() !== 'DELETE') {
      await route.continue();
      return;
    }
    if (!retrying) {
      retrying = true;
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        json: { error: { code: 'xv-conflict', message: 'Delete conflict.', hint: 'Retry.' } },
      });
      return;
    }
    await retryGate;
    await route.continue();
  });

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret late-ok' }).check();
  await page.getByRole('checkbox', { name: 'Select secret late-failed' }).check();
  await page.locator('#bulk-delete-secrets').click();
  await page.locator('#confirm-delete').click();
  const action = page.locator('#secret-error');
  await expect(action.getByRole('button', { name: 'Retry failed' })).toBeVisible();

  await action.getByRole('button', { name: 'Retry failed' }).click();
  await action.getByRole('button', { name: 'Dismiss error' }).click();
  await expect(action).toBeHidden();
  expect(await action.evaluate((panel) => (
    [...panel.querySelectorAll('button')].every((button) => button.onclick === null)
  ))).toBe(true);

  releaseRetry();
  await page.waitForTimeout(200);
  await expect(action).toBeHidden();
  const operations = await page.evaluate(() => Object.values(
    globalThis.__xvTestStoreSnapshot().operations || {},
  ));
  expect(operations.some(({ durable }) => durable)).toBe(false);
});

test('dismissed stale retry clears handlers and ignores its late failure', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'late-refresh');

  let listAttempt = 0;
  let releaseRetry;
  const retryGate = new Promise((resolve) => { releaseRetry = resolve; });
  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue();
      return;
    }
    listAttempt++;
    if (listAttempt > 1) await retryGate;
    await route.fulfill({
      status: 503,
      contentType: 'application/json',
      json: { error: { code: 'xv-offline', message: 'Late refresh failure.', hint: 'Retry.' } },
    });
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();
  const refresh = page.locator('#secret-refresh-error');
  await expect(refresh.getByRole('button', { name: 'Retry' })).toBeVisible();
  await refresh.getByRole('button', { name: 'Retry' }).click();
  await refresh.getByRole('button', { name: 'Dismiss error' }).click();
  await expect(refresh).toBeHidden();
  expect(await refresh.evaluate((panel) => (
    [...panel.querySelectorAll('button')].every((button) => button.onclick === null)
  ))).toBe(true);

  releaseRetry();
  await page.waitForTimeout(200);
  await expect(refresh).toBeHidden();
});

test('failed workspace activation preserves both current-scope stale refresh owners', async ({ page, baseURL }) => {
  await page.goto(baseURL);

  let failOldSecrets = true;
  let failOldFiles = true;
  let releaseActivation;
  let markActivationStarted;
  const activationGate = new Promise((resolve) => { releaseActivation = resolve; });
  const activationStarted = new Promise((resolve) => { markActivationStarted = resolve; });
  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() === 'GET' && failOldSecrets) {
      failOldSecrets = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Current secrets are stale.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/files?**', async (route) => {
    if (route.request().method() === 'GET' && failOldFiles) {
      failOldFiles = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Current files are stale.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/workspaces/activate', async (route) => {
    markActivationStarted();
    await activationGate;
    await route.fulfill({
      status: 503,
      contentType: 'application/json',
      json: { error: { code: 'xv-backend-unavailable', message: 'Activation failed.', hint: 'Retry later.' } },
    });
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.getByRole('button', { name: 'Refresh files' }).click();
  const secretPanel = page.locator('#secret-refresh-error');
  const filePanel = page.locator('#file-refresh-error');
  expect(await secretPanel.evaluate((panel) => panel.hidden)).toBe(false);
  await expect(filePanel).toBeVisible();

  await page.locator('#workspace-select').selectOption('sandbox');
  await activationStarted;
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  expect(await secretPanel.evaluate((panel) => panel.hidden)).toBe(false);
  expect(await filePanel.evaluate((panel) => panel.hidden)).toBe(false);
  expect(await page.evaluate(() => [
    document.querySelector('#secret-refresh-error .error-retry').onclick !== null,
    document.querySelector('#file-refresh-error .error-retry').onclick !== null,
  ])).toEqual([true, true]);

  releaseActivation();
  await expect(page.locator('#context-error')).toContainText('Activation failed');
  expect(await secretPanel.evaluate((panel) => panel.hidden)).toBe(false);
  expect(await filePanel.evaluate((panel) => panel.hidden)).toBe(false);
  expect(await page.evaluate(() => [
    document.querySelector('#secret-refresh-error .error-retry').onclick !== null,
    document.querySelector('#file-refresh-error .error-retry').onclick !== null,
  ])).toEqual([true, true]);

  await page.evaluate(async () => Promise.all([
    document.querySelector('#secret-refresh-error .error-retry').onclick(),
    document.querySelector('#file-refresh-error .error-retry').onclick(),
  ]));
  await expect.poll(() => secretPanel.evaluate((panel) => panel.hidden)).toBe(true);
  await expect.poll(() => filePanel.evaluate((panel) => panel.hidden)).toBe(true);
});

test('in-flight current refresh settles and retries after workspace activation failure', async ({ page, baseURL }) => {
  await page.goto(baseURL);

  let secretRefreshRequests = 0;
  let releaseRefresh;
  let markRefreshStarted;
  const refreshGate = new Promise((resolve) => { releaseRefresh = resolve; });
  const refreshStarted = new Promise((resolve) => { markRefreshStarted = resolve; });
  await page.route('**/api/secrets?**', async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue();
      return;
    }
    secretRefreshRequests++;
    if (secretRefreshRequests === 1) {
      markRefreshStarted();
      await refreshGate;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Refresh settled after activation failure.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/workspaces/activate', async (route) => {
    await route.fulfill({
      status: 503,
      contentType: 'application/json',
      json: { error: { code: 'xv-backend-unavailable', message: 'Activation failed.', hint: 'Retry later.' } },
    });
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();
  await refreshStarted;
  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#context-error')).toContainText('Activation failed');
  releaseRefresh();

  const panel = page.locator('#secret-refresh-error');
  await expect(panel).toContainText('Refresh settled after activation failure');
  await expect(page.locator('#secret-item-count')).not.toContainText('Loading');
  await panel.getByRole('button', { name: 'Retry' }).click();
  await expect(panel).toBeHidden();
  expect(secretRefreshRequests).toBe(2);
});

test('workspace transition clears both refresh owners before delayed new-scope files settle', async ({ page, baseURL }) => {
  await page.goto(baseURL);

  let failOldSecrets = true;
  let failOldFiles = true;
  let sandboxFileRequests = 0;
  let releaseActivation;
  let markActivationStarted;
  const activationGate = new Promise((resolve) => { releaseActivation = resolve; });
  const activationStarted = new Promise((resolve) => { markActivationStarted = resolve; });
  let releaseSandboxFiles;
  let markSandboxFilesStarted;
  const sandboxFilesGate = new Promise((resolve) => { releaseSandboxFiles = resolve; });
  const sandboxFilesStarted = new Promise((resolve) => { markSandboxFilesStarted = resolve; });

  await page.route('**/api/secrets?**', async (route) => {
    const url = new URL(route.request().url());
    if (
      route.request().method() === 'GET'
      && url.searchParams.get('vault') === 'playwright'
      && failOldSecrets
    ) {
      failOldSecrets = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Old secret refresh failed.', hint: 'Retry.' } },
      });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/files?**', async (route) => {
    const url = new URL(route.request().url());
    const vault = url.searchParams.get('vault');
    if (route.request().method() === 'GET' && vault === 'playwright' && failOldFiles) {
      failOldFiles = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        json: { error: { code: 'xv-offline', message: 'Old file refresh failed.', hint: 'Retry.' } },
      });
      return;
    }
    if (route.request().method() === 'GET' && vault === 'sandbox') {
      sandboxFileRequests++;
      markSandboxFilesStarted();
      await sandboxFilesGate;
      await route.fulfill({ status: 200, contentType: 'application/json', json: [] });
      return;
    }
    await route.continue();
  });
  await page.route('**/api/workspaces/activate', async (route) => {
    markActivationStarted();
    await activationGate;
    await route.continue();
  });

  await page.getByRole('button', { name: 'Refresh secrets' }).click();
  await expect(page.locator('#secret-refresh-error')).toBeVisible();
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.getByRole('button', { name: 'Refresh files' }).click();
  await expect(page.locator('#file-refresh-error')).toBeVisible();

  await page.evaluate(() => {
    const secretRetry = document.querySelector('#secret-refresh-error .error-retry');
    const fileRetry = document.querySelector('#file-refresh-error .error-retry');
    globalThis.__staleRefreshRetries = [secretRetry.onclick, fileRetry.onclick];
    secretRetry.disabled = true;
    fileRetry.disabled = true;
  });

  await page.locator('#workspace-select').selectOption('sandbox');
  await activationStarted;
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  expect(await page.locator('#secret-refresh-error').evaluate((panel) => panel.hidden)).toBe(false);
  expect(await page.locator('#file-refresh-error').evaluate((panel) => panel.hidden)).toBe(false);
  releaseActivation();
  await sandboxFilesStarted;

  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  await expect(page.locator('#secret-refresh-error')).toBeHidden();
  await expect(page.locator('#file-refresh-error')).toBeHidden();
  expect(await page.evaluate(() => (
    [...document.querySelectorAll(
      '#secret-refresh-error button, #file-refresh-error button',
    )].every((button) => button.onclick === null && button.disabled === false)
  ))).toBe(true);

  const requestsBeforeStaleRetries = sandboxFileRequests;
  expect(await page.evaluate(async () => Promise.all(
    globalThis.__staleRefreshRetries.map((retry) => retry()),
  ))).toEqual([false, false]);
  expect(sandboxFileRequests).toBe(requestsBeforeStaleRetries);

  releaseSandboxFiles();
  await expect(page.locator('#file-list-summary')).toContainText('Files remain encrypted');
});
