import { test, expect } from './fixtures.js';

async function createSecret(page, name, value) {
  await page.getByRole('button', { name: 'New secret' }).click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill(value);
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

async function deleteSecret(page, name) {
  await page.getByRole('button', { name: `Edit secret ${name}` }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  const confirmation = page.getByRole('dialog', { name: 'Delete secret?' });
  await expect(confirmation).toContainText('local');
  await expect(confirmation).toContainText('playwright');
  await expect(confirmation).toContainText(name);
  await expect(confirmation).toContainText('restored from Trash');
  await confirmation.getByRole('button', { name: 'Delete secret' }).click();
}

test('delete Undo, Trash conflict, and typed-name purge are safe and persistent', async ({ page, appUrl }) => {
  await page.goto(appUrl);
  await createSecret(page, 'recover-me', 'first value');

  await deleteSecret(page, 'recover-me');
  const notice = page.locator('#action-notice');
  await expect(notice).toContainText('moved to Trash');
  await notice.getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret recover-me' })).toBeVisible();

  await deleteSecret(page, 'recover-me');
  await createSecret(page, 'recover-me', 'replacement value');

  const trashTab = page.getByRole('tab', { name: 'Trash' });
  await trashTab.click();
  await expect(trashTab).toHaveAttribute('aria-selected', 'true');
  const trashRow = page.getByRole('row', { name: /recover-me/ }).first();
  await expect(trashRow).toContainText(/Deleted/i);
  await trashRow.getByRole('button', { name: 'Restore recover-me' }).click();
  await expect(page.locator('#trash-error')).toBeVisible();
  await expect(page.locator('#trash-error')).toContainText(/conflicts with existing data/i);
  await expect(trashRow).toBeVisible();

  await trashRow.getByRole('button', { name: 'Purge recover-me' }).click();
  const purge = page.getByRole('dialog', { name: 'Permanently purge recover-me?' });
  const confirmation = purge.getByLabel('Type recover-me to confirm');
  const purgeButton = purge.getByRole('button', { name: 'Permanently purge' });
  await expect(purgeButton).toBeDisabled();
  await confirmation.fill('recover');
  await expect(purgeButton).toBeDisabled();
  await confirmation.fill('recover-me');
  await expect(purgeButton).toBeEnabled();
  await purgeButton.click();
  await expect(page.getByRole('row', { name: /recover-me/ })).toHaveCount(0);
});
