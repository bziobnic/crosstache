import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function createSecret(page, name, value) {
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill(value);
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

async function deleteSecret(page, name, vault) {
  await page.getByRole('button', { name: `Edit secret ${name}` }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  const confirmation = page.getByRole('dialog', { name: 'Delete secret?' });
  await expect(confirmation).toContainText('local');
  await expect(confirmation).toContainText(vault);
  await expect(confirmation).toContainText(name);
  await expect(confirmation).toContainText('restored from Trash');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await confirmation.getByRole('button', { name: 'Delete secret' }).click();
}

test('delete Undo, Trash conflict, and typed-name purge are safe and persistent', async ({ page, baseURL, vault }) => {
  await page.goto(baseURL);
  await createSecret(page, 'recover-me', 'first value');

  await deleteSecret(page, 'recover-me', vault);
  const notice = page.locator('#action-notice');
  await expect(notice).toContainText('moved to Trash');
  await notice.getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret recover-me' })).toBeVisible();

  await deleteSecret(page, 'recover-me', vault);
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
  await expectNoSeriousOrCriticalAxeViolations(page);

  await trashRow.getByRole('button', { name: 'Purge recover-me' }).click();
  const purge = page.getByRole('dialog', { name: 'Permanently purge recover-me?' });
  const confirmation = purge.getByLabel('Type recover-me to confirm');
  const purgeButton = purge.getByRole('button', { name: 'Permanently purge' });
  await expect(purgeButton).toBeDisabled();
  await confirmation.fill('recover');
  await expect(purgeButton).toBeDisabled();
  await confirmation.fill('recover-me');
  await expect(purgeButton).toBeEnabled();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await purgeButton.click();
  await expect(page.getByRole('row', { name: /recover-me/ })).toHaveCount(0);
});

test('persistent Undo is inaccessible while a confirmation modal is open', async ({ page, baseURL, vault }) => {
  await page.goto(baseURL);
  await createSecret(page, 'first-delete', 'first value');
  await deleteSecret(page, 'first-delete', vault);
  await expect(page.locator('#action-notice')).toContainText('moved to Trash');

  await createSecret(page, 'second-delete', 'second value');
  await page.getByRole('button', { name: 'Edit secret second-delete' }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();

  await expect(page.getByRole('dialog', { name: 'Delete secret?' })).toBeVisible();
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expectNoSeriousOrCriticalAxeViolations(page);
  const undo = page.locator('#undo-delete');
  await expect.poll(() => undo.evaluate((element) => element.closest('[inert]')?.tagName)).toBe('MAIN');
  expect(await undo.evaluate((element) => {
    element.focus();
    return document.activeElement === element;
  })).toBe(false);
  await page.getByRole('dialog', { name: 'Delete secret?' }).getByRole('button', { name: 'Cancel' }).click();
});

test('file deletion identifies the exact context and warns that recovery is unavailable', async ({ page, baseURL, vault }) => {
  await page.goto(baseURL);
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.locator('#file-input').setInputFiles({
    name: 'delete-me.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('delete me'),
  });
  await expect(page.getByRole('link', { name: 'delete-me.txt' })).toBeVisible();

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Select file delete-me.txt' }).check();
  await page.locator('#bulk-delete-files').click();

  const confirmation = page.getByRole('dialog', { name: 'Delete file?' });
  await expect(confirmation).toContainText('local');
  await expect(confirmation).toContainText(vault);
  await expect(confirmation).toContainText('delete-me.txt');
  await expect(confirmation).toContainText('Recovery is unavailable for files on local.');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await confirmation.getByRole('button', { name: 'Delete file' }).click();
  await expect(page.getByRole('row', { name: /delete-me\.txt/ })).toHaveCount(0);
});
