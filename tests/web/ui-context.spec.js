import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

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
