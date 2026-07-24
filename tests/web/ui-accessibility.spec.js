import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function createSecret(page, name) {
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill('selection value');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

test('secret sheet traps focus, guards Escape, and restores the invoker', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await page.locator('#new-secret').click();
  const dialog = page.getByRole('dialog', { name: 'New secret' });
  const name = page.locator('#secret-form input[name="name"]');
  await expect(dialog).toBeVisible();
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expect(name).toBeFocused();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await name.fill('draft');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(name).toHaveValue('draft');
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.locator('#new-secret')).toBeFocused();
});

test('discard confirmation ignores backdrop clicks until its original action completes', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expectNoSeriousOrCriticalAxeViolations(page);
  const name = page.locator('#secret-form input[name="name"]');
  await page.locator('#new-secret').click();
  await name.fill('draft');
  await page.keyboard.press('Escape');
  const confirmation = page.getByRole('dialog', { name: 'Discard changes?' });
  await expect(confirmation).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.mouse.click(20, 200);
  await expect(confirmation).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(name).toBeFocused();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.locator('#new-secret')).toBeFocused();
});

test('secret and file selection states have no serious or critical violations', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'selection-secret');

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret selection-secret' }).check();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.locator('#cancel-secret-selection').click();

  await page.getByRole('tab', { name: 'Files' }).click();
  await page.locator('#file-input').setInputFiles({
    name: 'selection-file.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('selection file'),
  });
  await expect(page.getByRole('link', { name: 'selection-file.txt' })).toBeVisible();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select file selection-file.txt' }).check();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('tab and partial visible-selection states have no serious or critical violations', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await createSecret(page, 'first-a11y-secret');
  await createSecret(page, 'second-a11y-secret');
  await page.getByRole('tab', { name: 'Secrets' }).focus();
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('ArrowLeft');
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select secret first-a11y-secret' }).check();
  await expect(page.locator('#select-all-secrets')).toHaveAttribute('aria-checked', 'mixed');
  await expectNoSeriousOrCriticalAxeViolations(page);
});
