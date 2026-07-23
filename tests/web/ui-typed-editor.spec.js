import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function openNewSecret(page, baseURL) {
  await page.goto(baseURL);
  await expect.poll(() => page.locator('#new-secret').evaluate((node) => typeof node.onclick)).toBe('function');
  await page.locator('#new-secret').click();
}

async function putSecret(page, name, body) {
  const result = await page.evaluate(async ({ secretName, requestBody }) => {
    const token = sessionStorage.getItem('xv.ui.token');
    const context = await fetch('/api/context', {
      headers: { Authorization: `Bearer ${token}` },
    }).then((response) => response.json());
    const scope = new URLSearchParams({
      alias: context.workspace.alias,
      backend: context.backend,
      vault: context.vault,
    });
    const response = await fetch(`/api/secrets/${encodeURIComponent(secretName)}?${scope}`, {
      method: 'PUT',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(requestBody),
    });
    return { ok: response.ok, text: await response.text() };
  }, { secretName: name, requestBody: body });
  expect(result.ok, result.text).toBe(true);
}

test('guided cards create plain and typed secrets with only selected fields', async ({ page, baseURL }) => {
  await openNewSecret(page, baseURL);
  const form = page.locator('#secret-form');
  await expect(page.getByRole('radio', { name: /Plain/ })).toBeChecked();
  await expect(page.getByRole('radio', { name: /login/ })).toBeVisible();
  await expect(form.locator('textarea[name="value"]')).toBeVisible();

  await page.getByRole('radio', { name: /login/ }).check();
  await expect(form.locator('textarea[name="value"]')).toBeHidden();
  await expect(form.getByLabel(/username/i)).toBeVisible();
  const password = form.getByRole('textbox', { name: /^password/i });
  await expect(password).toBeVisible();
  await expect(password).toHaveAttribute('aria-describedby', /field-help/);
  await expect(form.getByLabel(/connection-string/i)).toHaveCount(0);

  await form.locator('input[name="name"]').fill('guided-login');
  await form.getByLabel(/username/i).fill('alice');
  await password.fill('browser-password');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret guided-login' })).toBeVisible();

  await page.locator('#new-secret').click();
  await form.locator('input[name="name"]').fill('guided-plain');
  await form.locator('textarea[name="value"]').fill('plain-browser-value');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret guided-plain' })).toBeVisible();
});

test('chips suggestions folder autocomplete and expiry controls keep a durable draft', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'suggestion-source', {
    value: 'suggestion-value',
    folder: 'apps/prod',
    groups: ['ops', 'prod'],
  });
  await page.reload();
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await expect(page.locator('#folder-suggestions option[value="apps/prod"]')).toHaveCount(1);
  await expect(page.locator('#group-suggestions option[value="ops"]')).toHaveCount(1);
  await form.locator('input[name="name"]').fill('draft-controls');
  await form.locator('textarea[name="value"]').fill('secret');
  await form.locator('input[name="folder"]').fill('apps/prod');
  await form.locator('#group-entry').fill('ops');
  await form.locator('#group-entry').press('Enter');
  await expect(form.getByRole('button', { name: 'Remove group ops' })).toBeVisible();
  await expect(form.locator('input[name="groups"]')).toHaveValue('ops');
  await form.locator('input[name="expires_on"]').fill('2032-05-04');
  await expect(form.locator('input[name="expires_on"]')).toHaveAttribute('type', 'date');
  await page.getByRole('button', { name: 'No expiry' }).click();
  await expect(form.locator('input[name="expires_on"]')).toBeDisabled();
  await page.getByRole('button', { name: 'Clear expiry' }).click();
  await expect(form.locator('input[name="expires_on"]')).toBeEnabled();
  await expect(form.locator('input[name="expires_on"]')).toHaveValue('');
  await expect(form.locator('input[name="name"]')).toHaveValue('draft-controls');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('typed metadata saves preserve custom tags state and untouched protected fields', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'preserve-record', {
    value: JSON.stringify({ password: 'untouched-password', otp: 'untouched-otp' }),
    content_type: 'application/vnd.xv.record',
    folder: 'apps',
    groups: ['ops'],
    tags: {
      'xv-type': 'login',
      'f.username': 'alice',
      owner: 'payments',
    },
    enabled: false,
    not_before: '2031-04-03T00:00:00Z',
  });
  await page.reload();
  let saved;
  await page.route('**/api/secrets/preserve-record?**', async (route) => {
    if (route.request().method() !== 'PUT') return route.continue();
    saved = route.request().postDataJSON();
    return route.continue();
  });
  await page.getByRole('button', { name: 'Edit secret preserve-record' }).click();
  await expect(page.locator('#current-secret-type')).toHaveText('Current type: login');
  await page.getByRole('textbox', { name: /^username/i }).fill('bob');
  await page.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret preserve-record' })).toBeVisible();
  expect(saved).toMatchObject({
    enabled: false,
    not_before: '2031-04-03T00:00:00Z',
    tags: {
      'xv-type': 'login',
      'f.username': 'bob',
      owner: 'payments',
    },
  });
  expect(JSON.parse(saved.value)).toEqual({
    otp: 'untouched-otp',
    password: 'untouched-password',
  });
});

test('conversion preview confirmation and rename are isolated workflows', async ({ page, baseURL }) => {
  await openNewSecret(page, baseURL);
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill('workflow-source');
  await form.locator('textarea[name="value"]').fill('source-value');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await page.getByRole('button', { name: 'Edit secret workflow-source' }).click();
  await expect(page.locator('#current-secret-type')).toHaveText('Current type: Plain');

  let applyBody;
  await page.route('**/api/secrets/workflow-source/conversion/preview?**', async (route) => {
    const body = route.request().postDataJSON();
    if (!body.supplied_fields?.username) {
      return route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-invalid-argument',
            message: 'The target needs a username.',
            hint: 'Supply the highlighted field.',
            field: 'supplied_fields.username',
          },
        }),
      });
    }
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        dropped: ['legacy'],
        exposed: [],
        renamed: [],
        missing_required: [],
        requires_confirmation: true,
        source_revision: 'revision-one',
      }),
    });
  });
  await page.route('**/api/secrets/workflow-source/conversion?**', async (route) => {
    applyBody = route.request().postDataJSON();
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ secret: {}, summary: { dropped: ['legacy'] } }),
    });
  });
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByLabel('Conversion target').selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  const suppliedUsername = page.getByLabel('username (required for conversion)');
  await expect(suppliedUsername).toBeFocused();
  await expect(suppliedUsername).toHaveAttribute('aria-invalid', 'true');
  await suppliedUsername.fill('alice');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect(page.locator('#conversion-summary')).toContainText('Drops 1 field');
  await expect(page.getByRole('button', { name: 'Confirm conversion' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.getByRole('button', { name: 'Confirm conversion' }).click();
  expect(applyBody).toMatchObject({
    target: { kind: 'typed', target_type: 'login' },
    confirm_lossy: true,
    source_revision: 'revision-one',
    supplied_fields: { username: 'alice' },
  });

  await page.getByRole('button', { name: 'Edit secret workflow-source' }).click();
  let renameAttempts = 0;
  await page.route('**/api/secrets/workflow-source/rename?**', async (route) => {
    renameAttempts++;
    if (renameAttempts > 1) return route.continue();
    return route.fulfill({
      status: 409,
      contentType: 'application/json',
      body: JSON.stringify({
        error: {
          code: 'xv-rename-destination-exists',
          message: 'That name already exists.',
          hint: 'Choose a different name.',
          field: 'name',
        },
      }),
    });
  });
  await page.getByRole('button', { name: 'Rename secret' }).click();
  const renameName = page.getByLabel('New secret name');
  await renameName.fill('workflow-renamed');
  await page.getByRole('button', { name: 'Apply rename' }).click();
  await expect(renameName).toBeFocused();
  await expect(renameName).toHaveValue('workflow-renamed');
  await expect(renameName).toHaveAttribute('aria-describedby', /secret-form-error/);
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.getByRole('button', { name: 'Apply rename' }).click();
  await expect(page.getByRole('button', { name: 'Edit secret workflow-renamed' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit secret workflow-source' })).toHaveCount(0);
});

test('server field errors remain described and focus the matching draft control', async ({ page, baseURL }) => {
  await openNewSecret(page, baseURL);
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill('field-error');
  await form.locator('textarea[name="value"]').fill('still-here');
  await page.route('**/api/secrets/field-error?**', async (route) => {
    await route.fulfill({
      status: 400,
      contentType: 'application/json',
      body: JSON.stringify({
        error: {
          code: 'xv-invalid-argument',
          message: 'Choose a valid folder.',
          hint: 'Use a relative folder path.',
          field: 'folder',
        },
      }),
    });
  });
  await page.getByRole('button', { name: 'Create secret' }).click();
  const folder = form.locator('input[name="folder"]');
  await expect(folder).toBeFocused();
  await expect(folder).toHaveAttribute('aria-invalid', 'true');
  await expect(folder).toHaveAttribute('aria-describedby', /secret-form-error/);
  await expect(form.locator('textarea[name="value"]')).toHaveValue('still-here');
});
