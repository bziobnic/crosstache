import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function expectPlainValueControlsOutsideLabel(page) {
  const reveal = page.getByRole('button', { name: 'Reveal value', exact: true });
  const copy = page.getByRole('button', { name: 'Copy value', exact: true });
  await expect(reveal).toHaveAttribute('aria-describedby', 'value-protection-state protected-value-status');
  await expect(copy).toHaveAttribute('aria-describedby', 'value-protection-state protected-value-status');
  await expect(page.locator('#value-section label button')).toHaveCount(0);
}

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

async function seedRecord(page, name) {
  await expect.poll(() => page.locator('#workspace-select').inputValue()).not.toBe('');
  const response = await page.evaluate(async (recordName) => {
    const token = sessionStorage.getItem('xv.ui.token');
    const context = await fetch('/api/context', {
      headers: { Authorization: `Bearer ${token}` },
    }).then((result) => result.json());
    const scope = new URLSearchParams({
      alias: context.workspace.alias,
      backend: context.backend,
      vault: context.vault,
    });
    const result = await fetch(`/api/secrets/${encodeURIComponent(recordName)}?${scope}`, {
      method: 'PUT',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        value: JSON.stringify({ password: 'first-browser-value', 'connection-string': 'second-browser-value' }),
        content_type: 'application/vnd.xv.record',
        tags: { 'xv-type': 'database' },
      }),
    });
    return { ok: result.ok, body: await result.text() };
  }, name);
  expect(response.ok, response.body).toBe(true);
  await page.reload();
  await page.getByRole('button', { name: `Edit secret ${name}` }).click();
}

test('copy countdown clears only an unchanged clipboard without announcing the value', async ({ page, baseURL }) => {
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.stack || error.message));
  await installClipboard(page);
  await page.goto(baseURL);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  await createAndOpenSecret(page, 'copy-lifecycle', 'never-announce-this');
  await expectPlainValueControlsOutsideLabel(page);
  await page.clock.install();

  const status = page.locator('#protected-value-status');
  await page.getByRole('button', { name: 'Copy value', exact: true }).click();
  await expect(status).toHaveText('Value copied. Clipboard clears in 30 seconds.');
  await expect(status).not.toContainText('never-announce-this');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.evaluate(() => window.__testClipboard.set('newer-content'));
  await page.clock.runFor(30_000);
  await expect(status).toHaveText('Value clipboard clearing could not be confirmed.');
  await expect.poll(() => page.evaluate(() => window.__testClipboard.get())).toBe('newer-content');

  await page.getByRole('button', { name: 'Copy value', exact: true }).click();
  await page.clock.runFor(30_000);
  await expect(status).toHaveText('Value clipboard cleared.');
  await expect.poll(() => page.evaluate(() => window.__testClipboard.get())).toBe('');
});

test('reveal inactivity resets and hides on timeout, visibility, blur, close, and save', async ({ page, baseURL }) => {
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.stack || error.message));
  await installClipboard(page);
  await page.goto(baseURL);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  await createAndOpenSecret(page, 'reveal-lifecycle', 'short-lived-value');
  await expectPlainValueControlsOutsideLabel(page);
  await page.clock.install();

  const value = page.locator('#secret-form textarea[name="value"]');
  const reveal = page.getByRole('button', { name: 'Reveal value', exact: true });
  const status = page.locator('#protected-value-status');
  await reveal.click();
  await expect(value).toHaveValue('short-lived-value');
  await expect(page.getByRole('button', { name: 'Hide value', exact: true })).toBeVisible();
  await expect(status).toHaveText('Value revealed. Hides in 30 seconds.');
  await expect(status).not.toContainText('short-lived-value');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.clock.runFor(29_000);
  await value.dispatchEvent('pointerdown');
  await page.clock.runFor(29_000);
  await expect(value).toHaveValue('short-lived-value');
  await page.clock.runFor(1_000);
  await expect(value).toHaveValue('***************');
  await expect(reveal).toBeVisible();

  await reveal.click();
  const hide = page.getByRole('button', { name: 'Hide value', exact: true });
  await expect(hide).toBeVisible();
  await hide.click();
  await expect(value).toHaveValue('***************');
  await expect(reveal).toBeVisible();

  await reveal.click();
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect(value).toHaveValue('***************');
  await expect(reveal).toBeVisible();

  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
  });
  await reveal.click();
  await page.evaluate(() => window.dispatchEvent(new Event('blur')));
  await expect(value).toHaveValue('***************');
  await expect(reveal).toBeVisible();

  await reveal.click();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect(value).toHaveValue('');

  await page.getByRole('button', { name: 'Edit secret reveal-lifecycle' }).click();
  await page.getByRole('button', { name: 'Reveal value', exact: true }).click();
  await page.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect(value).toHaveValue('');
});

test('record protected fields have unique descriptions and announce field-specific countdowns', async ({ page, baseURL }) => {
  await installClipboard(page);
  await page.goto(baseURL);
  await seedRecord(page, 'record-accessibility');

  const inputs = page.locator('#record-fields input[data-field-kind="secret"]');
  await expect(inputs).toHaveCount(2);
  await expect(page.locator('#record-fields label button')).toHaveCount(0);
  const descriptions = await inputs.evaluateAll((elements) => elements.map((input) => {
    const describedBy = input.getAttribute('aria-describedby');
    return { describedBy, descriptionId: describedBy.split(' ')[0] };
  }));
  expect(new Set(descriptions.map(({ descriptionId }) => descriptionId)).size).toBe(2);
  for (const { describedBy, descriptionId } of descriptions) {
    expect(describedBy).toBe(`${descriptionId} protected-value-status`);
  }

  for (const name of ['password', 'connection-string']) {
    const input = page.locator(`#record-fields input[data-field-name="${name}"]`);
    const describedBy = await input.getAttribute('aria-describedby');
    await expect(page.getByRole('button', { name: `Reveal ${name}`, exact: true })).toHaveAttribute('aria-describedby', describedBy);
    await expect(page.getByRole('button', { name: `Copy ${name}`, exact: true })).toHaveAttribute('aria-describedby', describedBy);
  }

  await page.getByRole('button', { name: 'Reveal password', exact: true }).click();
  const status = page.locator('#protected-value-status');
  await expect(status).toHaveText('password revealed. Hides in 30 seconds.');
  await expect(status).not.toContainText('first-browser-value');
  const field = page.locator('#record-fields input[data-field-name="password"]');
  await expect(field).toHaveValue('first-browser-value');
  const stateId = (await field.getAttribute('aria-describedby')).split(' ')[0];
  await expect(page.locator(`#${stateId}`)).toContainText(/revealed/);
  await expectNoSeriousOrCriticalAxeViolations(page);
});
