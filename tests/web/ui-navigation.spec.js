import {
  test,
  expect,
  expectNoSeriousOrCriticalAxeViolations,
} from './fixtures.js';

async function createSecret(page, name) {
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill('navigation value');
  await page.getByRole('button', { name: 'Create secret' }).click();
}

test('tabs use roving focus and activate with arrows and boundaries', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  const secrets = page.getByRole('tab', { name: 'Secrets' });
  const files = page.getByRole('tab', { name: 'Files' });
  const trash = page.getByRole('tab', { name: 'Trash' });

  await secrets.focus();
  await page.keyboard.press('ArrowRight');
  await expect(files).toBeFocused();
  await expect(files).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('tabpanel', { name: 'Files' })).toBeVisible();
  await expect(page.getByRole('tabpanel', { name: 'Secrets' })).toBeHidden();

  await page.keyboard.press('End');
  await expect(trash).toBeFocused();
  await expect(trash).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Home');
  await expect(secrets).toBeFocused();
  await expect(secrets).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('#vault-tabs [role="tab"][tabindex="0"]')).toHaveCount(1);
});

test('desktop selection has one checkbox per row and a visible-scope mixed header', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 900, height: 760 });
  await page.goto(baseURL);
  await createSecret(page, 'alpha-visible');
  await createSecret(page, 'beta-visible');

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  const table = page.locator('#secrets-table');
  const rows = table.locator('tbody tr');
  await expect(rows).toHaveCount(2);
  for (const row of await rows.all()) {
    await expect(row.getByRole('checkbox')).toHaveCount(1);
    await expect(row.locator('button, a')).toHaveCount(0);
  }

  const header = table.getByRole('checkbox', { name: 'Select all 2 visible secrets' });
  await expect(header).toHaveAttribute('aria-checked', 'false');
  await table.getByRole('checkbox', { name: 'Select secret alpha-visible' }).check();
  await expect(header).toHaveAttribute('aria-checked', 'mixed');

  await page.getByLabel('Search secrets').fill('alpha');
  const scopedHeader = table.getByRole('checkbox', { name: 'Select all 1 visible secret' });
  await expect(scopedHeader).toHaveAttribute('aria-checked', 'true');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('desktop file selection removes duplicate row activation and scopes its header', async ({ page, baseURL }) => {
  await page.route('**/api/files?*', async (route) => route.fulfill({
    json: [
      { name: 'alpha.txt', size: 1, content_type: 'text/plain', last_modified: '2026-07-24T00:00:00Z' },
      { name: 'beta.txt', size: 2, content_type: 'text/plain', last_modified: '2026-07-24T00:00:00Z' },
    ],
  }));
  await page.setViewportSize({ width: 900, height: 760 });
  await page.goto(baseURL);
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();

  const table = page.locator('#files-table');
  for (const row of await table.locator('tbody tr').all()) {
    await expect(row.getByRole('checkbox')).toHaveCount(1);
    await expect(row.locator('button, a')).toHaveCount(0);
  }
  const header = table.getByRole('checkbox', { name: 'Select all 2 visible files' });
  await table.getByRole('checkbox', { name: 'Select file alpha.txt' }).check();
  await expect(header).toHaveAttribute('aria-checked', 'mixed');
});

test('Escape dismisses a higher modal before exiting selection', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'escape-secret');
  await page.getByRole('button', { name: 'Select', exact: true }).click();

  await page.locator('#help-open').click();
  await expect(page.getByRole('dialog', { name: 'Help' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Help' })).toBeHidden();
  await expect(page.locator('#secret-bulk-bar')).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(page.locator('#secret-bulk-bar')).toBeHidden();
  await expect(page.getByRole('button', { name: 'Select', exact: true })).toBeVisible();
});

test('selection state survives filtering and focus follows the active responsive surface', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 900, height: 760 });
  await page.goto(baseURL);
  await createSecret(page, 'focus-survivor');
  await createSecret(page, 'other-secret');
  await page.getByRole('button', { name: 'Select', exact: true }).click();

  const checkbox = page.getByRole('checkbox', { name: 'Select secret focus-survivor' });
  await checkbox.focus();
  await checkbox.check();
  await expect(checkbox).toBeFocused();
  await page.setViewportSize({ width: 390, height: 844 });
  const stackedCheckbox = page.locator('#secrets-stacked').getByRole('checkbox', {
    name: 'Select secret focus-survivor',
  });
  await expect(stackedCheckbox).toBeFocused();
  await page.getByLabel('Search secrets').fill('focus');
  await expect(page.getByLabel('Search secrets')).toBeFocused();
  await expect(stackedCheckbox).toBeChecked();
  await expect(page.locator('#secrets-stacked').getByRole('checkbox')).toHaveCount(1);
});
