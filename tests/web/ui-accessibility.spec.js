import AxeBuilder from '@axe-core/playwright';
import { test, expect } from './fixtures.js';

test('secret sheet traps focus, guards Escape, and restores the invoker', async ({ page, appUrl }) => {
  await page.goto(appUrl);
  await page.locator('#new-secret').click();
  const dialog = page.getByRole('dialog', { name: 'New secret' });
  const name = page.locator('#secret-form input[name="name"]');
  await expect(dialog).toBeVisible();
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expect(name).toBeFocused();
  await name.fill('draft');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(name).toHaveValue('draft');
  const axeResults = await new AxeBuilder({ page }).include('#drawer').analyze();
  expect(axeResults.violations).toEqual([]);
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.locator('#new-secret')).toBeFocused();
});

test('discard confirmation ignores backdrop clicks until its original action completes', async ({ page, appUrl }) => {
  await page.goto(appUrl);
  const name = page.locator('#secret-form input[name="name"]');
  await page.locator('#new-secret').click();
  await name.fill('draft');
  await page.keyboard.press('Escape');
  const confirmation = page.getByRole('dialog', { name: 'Discard changes?' });
  await expect(confirmation).toBeVisible();
  await page.mouse.click(20, 200);
  await expect(confirmation).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(name).toBeFocused();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.locator('#new-secret')).toBeFocused();
});
