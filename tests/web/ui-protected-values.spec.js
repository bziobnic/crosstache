import { test, expect } from './fixtures.js';

async function installClipboard(page) {
  await page.addInitScript(() => {
    let value = '';
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        readText: async () => value,
        writeText: async (next) => { value = next; },
      },
    });
    window.__testClipboard = {
      get: () => value,
      set: (next) => { value = next; },
    };
  });
}

async function createAndOpenSecret(page, name, value) {
  await expect.poll(() => page.locator('#new-secret').evaluate((element) => typeof element.onclick)).toBe('function');
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill(value);
  await page.getByRole('button', { name: 'Create secret' }).click();
  const edit = page.getByRole('button', { name: `Edit secret ${name}` });
  await expect(edit).toBeVisible();
  await edit.click();
}

test('copy countdown clears only an unchanged clipboard without announcing the value', async ({ page, appUrl }) => {
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.stack || error.message));
  await installClipboard(page);
  await page.goto(appUrl);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  await createAndOpenSecret(page, 'copy-lifecycle', 'never-announce-this');
  await page.clock.install();

  const status = page.locator('#protected-value-status');
  await page.getByRole('button', { name: 'Copy', exact: true }).click();
  await expect(status).toHaveText('Value copied. Clipboard clears in 30 seconds.');
  await expect(status).not.toContainText('never-announce-this');
  await page.evaluate(() => window.__testClipboard.set('newer-content'));
  await page.clock.runFor(30_000);
  await expect(status).toHaveText('Value clipboard clearing could not be confirmed.');
  await expect.poll(() => page.evaluate(() => window.__testClipboard.get())).toBe('newer-content');

  await page.getByRole('button', { name: 'Copy', exact: true }).click();
  await page.clock.runFor(30_000);
  await expect(status).toHaveText('Value clipboard cleared.');
  await expect.poll(() => page.evaluate(() => window.__testClipboard.get())).toBe('');
});

test('reveal inactivity resets and hides on timeout, visibility, blur, close, and save', async ({ page, appUrl }) => {
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.stack || error.message));
  await installClipboard(page);
  await page.goto(appUrl);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  await createAndOpenSecret(page, 'reveal-lifecycle', 'short-lived-value');
  await page.clock.install();

  const value = page.locator('#secret-form textarea[name="value"]');
  const reveal = page.getByRole('button', { name: 'Reveal', exact: true });
  const status = page.locator('#protected-value-status');
  await reveal.click();
  await expect(value).toHaveValue('short-lived-value');
  await expect(status).toHaveText('Value revealed. Hides in 30 seconds.');
  await expect(status).not.toContainText('short-lived-value');
  await page.clock.runFor(29_000);
  await value.dispatchEvent('pointerdown');
  await page.clock.runFor(29_000);
  await expect(value).toHaveValue('short-lived-value');
  await page.clock.runFor(1_000);
  await expect(value).toHaveValue('***************');

  await reveal.click();
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect(value).toHaveValue('***************');

  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
  });
  await reveal.click();
  await page.evaluate(() => window.dispatchEvent(new Event('blur')));
  await expect(value).toHaveValue('***************');

  await reveal.click();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect(value).toHaveValue('');

  await page.getByRole('button', { name: 'Edit secret reveal-lifecycle' }).click();
  await page.getByRole('button', { name: 'Reveal', exact: true }).click();
  await page.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect(value).toHaveValue('');
});
