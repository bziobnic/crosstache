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
    window.__conversionClipboard = () => value;
  });
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

test('conversion previews ignore stale responses and apply one immutable request snapshot', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'preview-race', { value: 'race-value' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret preview-race' }).click();

  const pending = [];
  await page.route('**/api/secrets/preview-race/conversion/preview?**', async (route) => {
    await new Promise((resolve) => pending.push({ route, resolve }));
  });
  await page.getByRole('button', { name: 'Convert type' }).click();
  const target = page.getByLabel('Conversion target');
  await target.selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect.poll(() => pending.length).toBe(1);
  await target.selectOption('api-key');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect.poll(() => pending.length).toBe(2);

  await pending[1].route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      dropped: ['newer-api-key-preview'],
      exposed: [],
      renamed: [],
      requires_confirmation: true,
      source_revision: 'revision-newer',
    }),
  });
  pending[1].resolve();
  await expect(page.locator('#conversion-summary')).toContainText('newer-api-key-preview');
  await pending[0].route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      dropped: ['stale-login-preview'],
      exposed: [],
      renamed: [],
      requires_confirmation: true,
      source_revision: 'revision-stale',
    }),
  });
  pending[0].resolve();
  await expect(page.locator('#conversion-summary')).not.toContainText('stale-login-preview');

  let applyBody;
  await page.route('**/api/secrets/preview-race/conversion?**', async (route) => {
    applyBody = route.request().postDataJSON();
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ secret: {}, summary: {} }),
    });
  });
  await page.getByRole('button', { name: 'Confirm conversion' }).click();
  expect(applyBody).toEqual({
    target: { kind: 'typed', target_type: 'api-key' },
    supplied_fields: {},
    confirm_lossy: true,
    source_revision: 'revision-newer',
  });
});

test('supplied protected fields are lifecycle-managed and edits invalidate confirmation', async ({ page, baseURL }) => {
  await installClipboard(page);
  await page.goto(baseURL);
  await putSecret(page, 'protected-conversion', { value: 'conversion-source' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret protected-conversion' }).click();
  await page.route('**/api/secrets/protected-conversion/conversion/preview?**', async (route) => {
    const body = route.request().postDataJSON();
    if (!body.supplied_fields?.password) {
      return route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-invalid-argument',
            message: 'Password is required.',
            hint: 'Supply the protected field.',
            field: 'supplied_fields.password',
          },
        }),
      });
    }
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        dropped: [],
        exposed: [],
        renamed: [],
        requires_confirmation: false,
        source_revision: 'protected-revision',
      }),
    });
  });
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByLabel('Conversion target').selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  const password = page.locator('#conversion-required-fields input[data-conversion-field="password"]');
  await expect(password).toHaveAttribute('data-field-kind', 'secret');
  await password.fill('conversion-protected-marker');
  expect(await password.evaluate((input) => ({
    connected: input.isConnected,
    value: input._protectedState.value,
    dirty: input._protectedState.dirty,
  }))).toEqual({
    connected: true,
    value: 'conversion-protected-marker',
    dirty: true,
  });
  await page.getByRole('button', { name: 'Copy password', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.__conversionClipboard())).toBe('conversion-protected-marker');
  await page.getByRole('button', { name: 'Hide password', exact: true }).click();
  await expect(password).toHaveValue('***************');
  await page.getByRole('button', { name: 'Reveal password', exact: true }).click();
  await expect(password).toHaveValue('conversion-protected-marker');
  await page.getByRole('button', { name: 'Rename secret' }).click();
  await expect(password).toBeHidden();
  expect(await password.evaluate((input) => input._protectedState.value)).toBe('conversion-protected-marker');
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect(page.getByRole('button', { name: 'Confirm conversion' })).toBeVisible();
  await password.fill('changed-after-preview');
  await expect(page.getByRole('button', { name: 'Confirm conversion' })).toBeHidden();

  await page.evaluate(() => {
    window.__detachedConversionInput = document.querySelector(
      '#conversion-required-fields input[data-conversion-field="password"]',
    );
  });
  await page.getByLabel('Conversion target').selectOption('api-key');
  await expect(password).toHaveCount(0);
  expect(await page.evaluate(() => ({
    connected: window.__detachedConversionInput.isConnected,
    value: window.__detachedConversionInput.value,
    protectedValue: window.__detachedConversionInput._protectedState?.value,
    hasStoredValue: window.__detachedConversionInput._protectedState?.hasStoredValue,
  }))).toEqual({
    connected: false,
    value: '',
    protectedValue: null,
    hasStoredValue: false,
  });
});

test('conversion failures and drawer close scrub detached protected supplied state', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'protected-failure', { value: 'conversion-source' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret protected-failure' }).click();
  await page.route('**/api/secrets/protected-failure/conversion/preview?**', async (route) => {
    const body = route.request().postDataJSON();
    if (!body.supplied_fields?.password) {
      return route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-invalid-argument',
            message: 'Password is required.',
            hint: 'Supply the protected field.',
            field: 'supplied_fields.password',
          },
        }),
      });
    }
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        dropped: [],
        exposed: [],
        renamed: [],
        requires_confirmation: false,
        source_revision: 'failure-revision',
      }),
    });
  });
  await page.route('**/api/secrets/protected-failure/conversion?**', async (route) => {
    await route.fulfill({
      status: 409,
      contentType: 'application/json',
      body: JSON.stringify({
        error: {
          code: 'xv-conversion-source-changed',
          message: 'The source changed.',
          hint: 'Preview again.',
          field: 'source_revision',
        },
      }),
    });
  });
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByLabel('Conversion target').selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  let password = page.locator('#conversion-required-fields input[data-conversion-field="password"]');
  await password.fill('apply-failure-marker');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await page.evaluate(() => {
    window.__failedConversionInput = document.querySelector(
      '#conversion-required-fields input[data-conversion-field="password"]',
    );
  });
  await page.getByRole('button', { name: 'Confirm conversion' }).click();
  await expect(page.locator('#secret-form-error')).toBeVisible();
  await expect(page.locator('#conversion-workflow')).not.toHaveAttribute('inert', '');
  await expect(page.getByLabel('Conversion target')).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Preview conversion' })).toBeEnabled();
  expect(await page.evaluate(() => ({
    connected: window.__failedConversionInput.isConnected,
    value: window.__failedConversionInput.value,
    protectedValue: window.__failedConversionInput._protectedState.value,
    hasStoredValue: window.__failedConversionInput._protectedState.hasStoredValue,
  }))).toEqual({
    connected: false,
    value: '',
    protectedValue: null,
    hasStoredValue: false,
  });

  await page.getByRole('button', { name: 'Preview conversion' }).click();
  password = page.locator('#conversion-required-fields input[data-conversion-field="password"]');
  await password.fill('close-marker');
  await page.evaluate(() => {
    window.__closedConversionInput = document.querySelector(
      '#conversion-required-fields input[data-conversion-field="password"]',
    );
  });
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard changes' }).click();
  expect(await page.evaluate(() => ({
    connected: window.__closedConversionInput.isConnected,
    value: window.__closedConversionInput.value,
    protectedValue: window.__closedConversionInput._protectedState.value,
    hasStoredValue: window.__closedConversionInput._protectedState.hasStoredValue,
  }))).toEqual({
    connected: false,
    value: '',
    protectedValue: null,
    hasStoredValue: false,
  });
});

test('preview failure scrubs protected supplied values from DOM state and the central draft', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'preview-scrub', { value: 'conversion-source' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret preview-scrub' }).click();
  await page.route('**/api/secrets/preview-scrub/conversion/preview?**', async (route) => {
    const body = route.request().postDataJSON();
    if (!body.supplied_fields?.password) {
      return route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-invalid-argument',
            message: 'Password is required.',
            field: 'supplied_fields.password',
          },
        }),
      });
    }
    return route.fulfill({
      status: 503,
      contentType: 'application/json',
      body: JSON.stringify({
        error: {
          code: 'xv-backend-unavailable',
          message: 'Preview failed.',
        },
      }),
    });
  });

  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByLabel('Conversion target').selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  const password = page.locator('#conversion-required-fields input[data-conversion-field="password"]');
  await password.fill('draft-must-be-scrubbed');
  expect(await page.evaluate(
    () => window.__xvTestStoreSnapshot().draft.working.conversion.supplied_fields,
  )).toEqual({ password: 'draft-must-be-scrubbed' });

  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect(page.locator('#secret-form-error')).toContainText('Preview failed.');
  expect(await password.evaluate((input) => ({
    value: input.value,
    protectedValue: input._protectedState.value,
    hasStoredValue: input._protectedState.hasStoredValue,
  }))).toEqual({
    value: '',
    protectedValue: null,
    hasStoredValue: false,
  });
  expect(await page.evaluate(
    () => window.__xvTestStoreSnapshot().draft.working.conversion.supplied_fields,
  )).toEqual({});
});

test('delayed conversion apply locks its full form and reconciles the immutable operation', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'pending-conversion', { value: 'conversion-source' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret pending-conversion' }).click();
  await page.route('**/api/secrets/pending-conversion/conversion/preview?**', async (route) => {
    const body = route.request().postDataJSON();
    if (!body.supplied_fields?.password) {
      return route.fulfill({
        status: 400,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-invalid-argument',
            message: 'Password is required.',
            field: 'supplied_fields.password',
          },
        }),
      });
    }
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        dropped: [],
        exposed: [],
        renamed: [],
        requires_confirmation: false,
        source_revision: 'pending-revision',
      }),
    });
  });
  let applyBody;
  let releaseApply;
  await page.route('**/api/secrets/pending-conversion/conversion?**', async (route) => {
    applyBody = route.request().postDataJSON();
    await new Promise((resolve) => { releaseApply = async () => {
      await route.fulfill({
        contentType: 'application/json',
        body: JSON.stringify({ secret: {}, summary: {} }),
      });
      resolve();
    }; });
  });

  await page.getByRole('button', { name: 'Convert type' }).click();
  const target = page.getByLabel('Conversion target');
  await target.selectOption('login');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  const password = page.locator('#conversion-required-fields input[data-conversion-field="password"]');
  await password.fill('immutable-password');
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await page.getByRole('button', { name: 'Confirm conversion' }).click();
  await expect.poll(() => typeof releaseApply).toBe('function');

  const workflow = page.locator('#conversion-workflow');
  await expect(workflow).toHaveAttribute('inert', '');
  for (const control of [
    target,
    password,
    page.getByRole('button', { name: 'Hide password', exact: true }),
    page.getByRole('button', { name: 'Copy password', exact: true }),
    page.getByRole('button', { name: 'Preview conversion' }),
    page.getByRole('button', { name: 'Confirm conversion' }),
  ]) {
    await expect(control).toBeDisabled();
  }

  await page.evaluate(() => {
    const targetControl = document.querySelector('#conversion-target');
    targetControl.value = 'api-key';
    targetControl.dispatchEvent(new Event('change', { bubbles: true }));
    const supplied = document.querySelector(
      '#conversion-required-fields input[data-conversion-field="password"]',
    );
    supplied.value = 'attempted-tamper';
    supplied.dispatchEvent(new InputEvent('input', { bubbles: true }));
  });
  expect(applyBody).toEqual({
    target: { kind: 'typed', target_type: 'login' },
    supplied_fields: { password: 'immutable-password' },
    confirm_lossy: true,
    source_revision: 'pending-revision',
  });

  let listRefreshes = 0;
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (request.method() === 'GET' && url.pathname === '/api/secrets') listRefreshes++;
  });
  await releaseApply();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect.poll(() => listRefreshes).toBeGreaterThan(0);
});

test('rename and conversion fields participate in drawer and context navigation guards', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await putSecret(page, 'workflow-draft', { value: 'workflow-value' });
  await page.reload();
  await page.getByRole('button', { name: 'Edit secret workflow-draft' }).click();
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByLabel('Conversion target').selectOption('login');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(page.getByLabel('Conversion target')).toHaveValue('login');

  await page.getByRole('button', { name: 'Rename secret' }).click();
  await page.getByLabel('New secret name').fill('workflow-draft-renamed');
  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(page.getByLabel('New secret name')).toHaveValue('workflow-draft-renamed');
  await expect(page.locator('#workspace-select')).not.toHaveValue('sandbox');

  let releasePreview;
  await page.route('**/api/secrets/workflow-draft/conversion/preview?**', async (route) => {
    await new Promise((resolve) => { releasePreview = async () => {
      await route.fulfill({
        contentType: 'application/json',
        body: JSON.stringify({
          dropped: ['must-remain-stale'],
          exposed: [],
          renamed: [],
          requires_confirmation: true,
          source_revision: 'stale-after-context',
        }),
      });
      resolve();
    }; });
  });
  await page.getByRole('button', { name: 'Convert type' }).click();
  await page.getByRole('button', { name: 'Preview conversion' }).click();
  await expect.poll(() => typeof releasePreview).toBe('function');
  await page.locator('#workspace-select').selectOption('sandbox');
  await page.getByRole('button', { name: 'Discard changes' }).click();
  await expect(page.locator('#context-line')).toContainText('sandbox');
  await releasePreview();
  await expect(page.locator('#drawer')).toBeHidden();
  await expect(page.getByRole('button', { name: 'Confirm conversion' })).toBeHidden();
});
