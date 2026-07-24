import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

async function createSecret(page, name, folder = '') {
  await page.locator('#new-secret').click();
  await page.locator('#secret-form input[name="name"]').fill(name);
  await page.locator('#secret-form textarea[name="value"]').fill('never-search-this-value');
  if (folder) await page.locator('#secret-form input[name="folder"]').fill(folder);
  await page.getByRole('button', { name: 'Create secret' }).click();
  await expect(page.getByRole('button', { name: `Edit secret ${name}` })).toBeVisible();
}

test('palette searches loaded metadata without retaining or exposing secret values', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await createSecret(page, 'production-login', 'prod');
  await createSecret(page, 'other-login', 'other');
  await page.getByRole('treeitem', { name: /other,/i }).click();
  await expect(page.getByRole('button', { name: 'Edit secret production-login' })).toBeHidden();

  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  const palette = page.getByRole('dialog', { name: 'Commands' });
  const query = palette.getByRole('combobox', { name: 'Search commands and vault metadata' });
  await expect(query).toBeFocused();
  await expect(query).toHaveAttribute('aria-expanded', 'true');
  await expect(query).toHaveAttribute('aria-controls', 'commands-results');
  await query.fill('production');
  const result = palette.getByRole('option', { name: /production-login.*Secrets.*local \/ playwright/i });
  await expect(result).toBeVisible();
  await expect(result).toHaveAttribute('tabindex', '-1');
  await query.press('End');
  await expect(query).toBeFocused();
  await expect(query).toHaveAttribute('aria-activedescendant', /command-result-/);
  await query.press('Home');
  await expect(query).toBeFocused();
  await expect(palette).not.toContainText('never-search-this-value');
  expect(page.url()).not.toContain('production');
  expect(await page.evaluate(() => JSON.stringify({
    local: { ...localStorage },
    session: { ...sessionStorage },
  }))).not.toContain('production');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await result.click();
  await expect(page.getByRole('dialog', { name: 'production-login' })).toBeVisible();

  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  await expect(query).toHaveValue('');
  await expect(query).toHaveAttribute('aria-expanded', 'true');
});

test('shortcuts respect editable controls and local surface state', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await page.keyboard.press('/');
  await expect(page.locator('#search')).toBeFocused();
  await page.locator('#search').fill('typing-here');
  await page.keyboard.press('/');
  await expect(page.locator('#search')).toHaveValue('typing-here/');
  await page.locator('#search').evaluate((input) => {
    for (const init of [
      { key: 'k', metaKey: true },
      { key: 'n', ctrlKey: true },
      { key: 'Escape' },
      { key: '/', isComposing: true },
      { key: '/', repeat: true },
      { key: 'k', metaKey: true, shiftKey: true },
    ]) input.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, ...init }));
  });
  await expect(page.getByRole('dialog', { name: 'Commands' })).toBeHidden();

  await page.locator('#search').blur();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+N' : 'Control+N');
  await expect(page.getByRole('dialog', { name: 'New secret' })).toBeVisible();
  await page.getByRole('button', { name: 'Cancel' }).click();

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await page.locator('#select-secrets').focus();
  await expect(page.locator('#secret-bulk-bar')).toBeVisible();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  await expect(page.getByRole('dialog', { name: 'Commands' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Commands' })).toBeHidden();
  await expect(page.locator('#secret-bulk-bar')).toBeVisible();
  await page.locator('#action-notice').evaluate((notice) => { notice.hidden = false; });
  await page.locator('#dismiss-action-notice').focus();
  await page.keyboard.press('Escape');
  await expect(page.locator('#action-notice')).toBeHidden();
  await expect(page.locator('#secret-bulk-bar')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('#secret-bulk-bar')).toBeHidden();
});

test('tab keyboard navigation skips unavailable tabs and preserves guarded drafts', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  const secretsTab = page.getByRole('tab', { name: 'Secrets' });
  await secretsTab.focus();
  await page.keyboard.press('End');
  await expect(page.getByRole('tab', { name: 'Trash' })).toBeFocused();
  await expect(page.getByRole('tab', { name: 'Trash' })).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Home');
  await expect(secretsTab).toBeFocused();
  await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('tab', { name: 'Files' })).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(secretsTab).toBeFocused();

  await page.locator('#new-secret').click();
  await page.locator('#secret-form input[name="name"]').fill('preserved-command-draft');
  await page.evaluate(() => document.querySelector('#commands-open').click());
  const palette = page.getByRole('dialog', { name: 'Commands' });
  await palette.getByRole('combobox', { name: 'Search commands and vault metadata' }).fill('sandbox');
  await palette.getByRole('option', { name: /sandbox.*Context.*local \/ sandbox/i }).click();
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(page.locator('#secret-form input[name="name"]')).toHaveValue('preserved-command-draft');
  await expect(page.locator('#workspace-select')).toHaveValue('playwright');
});

test('workspace, file, folder, and trash results use truthful exact targets', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await createSecret(page, 'folder-target', 'prod');
  await page.locator('#file-input').setInputFiles({
    name: 'palette-file.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('palette file'),
  });
  await page.getByRole('tab', { name: 'Files' }).click();
  await expect(page.getByRole('link', { name: 'palette-file.txt' })).toBeVisible();

  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  let palette = page.getByRole('dialog', { name: 'Commands' });
  await palette.getByRole('combobox').fill('palette-file.txt');
  await palette.getByRole('option', { name: /palette-file\.txt.*Files.*local \/ playwright/i }).click();
  await expect(page.getByRole('link', { name: 'palette-file.txt' })).toBeFocused();

  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  palette = page.getByRole('dialog', { name: 'Commands' });
  await palette.getByRole('combobox').fill('prod');
  await palette.getByRole('option', { name: /^prod.*Secrets.*local \/ playwright/i }).click();
  await expect(page.getByRole('treeitem', { name: /prod,/i })).toHaveAttribute('aria-selected', 'true');

  const activation = page.waitForRequest((request) => (
    request.method() === 'POST' && request.url().includes('/api/workspaces/activate')
  ));
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  palette = page.getByRole('dialog', { name: 'Commands' });
  await palette.getByRole('combobox').fill('sandbox');
  await palette.getByRole('option', { name: /sandbox.*Context.*local \/ sandbox/i }).click();
  expect((await activation).postDataJSON()).toEqual({
    alias: 'sandbox',
    backend: 'local',
    vault: 'sandbox',
  });
  await expect(page.locator('#context-line')).toContainText('local / sandbox');

  await page.getByRole('tab', { name: 'Trash' }).click();
  await page.locator('body').press('/');
  await expect(page.getByRole('tab', { name: 'Trash' })).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  palette = page.getByRole('dialog', { name: 'Commands' });
  await expect(palette.getByRole('option', { name: /Search secrets|Search files/i })).toHaveCount(0);
});

test('palette focus restores and narrow layouts keep one combobox focus owner', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 360, height: 720 });
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await page.locator('#help-open').focus();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  const query = page.locator('#commands-query');
  await expect(query).toBeFocused();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.keyboard.press('Escape');
  await expect(page.locator('#help-open')).toBeFocused();
  await expect(query).toHaveAttribute('aria-expanded', 'false');
  await expect(query).not.toHaveAttribute('aria-activedescendant', /.+/);
});

test('pending scoped work suppresses palette actions until the owner settles', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  let releaseSave;
  const saveGate = new Promise((resolve) => { releaseSave = resolve; });
  await page.route('**/api/secrets/pending-command?**', async (route) => {
    if (route.request().method() === 'PUT') await saveGate;
    await route.continue();
  });

  await page.locator('#new-secret').click();
  await page.locator('#secret-form input[name="name"]').fill('pending-command');
  await page.locator('#secret-form textarea[name="value"]').fill('fixture-only');
  await page.getByRole('button', { name: 'Create secret' }).click();
  await page.waitForFunction(() => globalThis.__xvTestStoreSnapshot?.().savePending === true);
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+K' : 'Control+K');
  await expect(page.getByRole('dialog', { name: 'Commands' })).toBeHidden();

  releaseSave();
  await expect(page.getByRole('button', { name: 'Edit secret pending-command' })).toBeVisible();
});
