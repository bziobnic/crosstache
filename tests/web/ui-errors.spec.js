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
  await expect(page.locator('#secret-error')).toContainText('Stale');
  await expect(page.locator('#secret-error').getByRole('button', { name: 'Retry' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);

  failRefresh = false;
  await page.locator('#secret-error').getByRole('button', { name: 'Retry' }).click();
  await expect(page.locator('#secret-error')).toBeHidden();
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
