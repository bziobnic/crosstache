import { readFile } from 'node:fs/promises';
import path from 'node:path';
import {
  test,
  expect,
  expectNoSeriousOrCriticalAxeViolations,
} from './fixtures.js';

const workspace = path.resolve(import.meta.dirname, '../..');
const longName = `${'credential-'.repeat(10)}tail`;
const nestedFolder = 'teams/platform/production';
const longFileLeaf = `${'archive-'.repeat(13)}tail.txt`;
const longFileName = `${nestedFolder}/${longFileLeaf}`;

async function createSecret(page, name, folder = nestedFolder) {
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(name);
  await form.locator('textarea[name="value"]').fill('responsive value');
  await form.locator('input[name="folder"]').fill(folder);
  await page.getByRole('button', { name: 'Create secret' }).click();
}

async function createLongSecret(page) {
  await createSecret(page, longName);
}

async function expectNoHorizontalOverflow(page) {
  await expect.poll(() => page.evaluate(() => ({
    body: document.body.scrollWidth <= document.body.clientWidth,
    root: document.documentElement.scrollWidth <= document.documentElement.clientWidth,
  }))).toEqual({ body: true, root: true });
}

async function uploadDirect(page, {
  name = longFileLeaf,
  destination = nestedFolder,
  body = 'responsive file',
} = {}) {
  await page.evaluate(async ({ name: fileName, destination: folder, body: contents }) => {
    const context = window.__xvTestStoreSnapshot().context;
    const query = new URLSearchParams({
      alias: context.workspace.alias,
      backend: context.backend,
      vault: context.vault,
      policy: 'replace',
      destination: folder,
    });
    const form = new FormData();
    form.append('file', new File([contents], fileName, { type: 'text/plain' }));
    const response = await fetch(`/api/files?${query}`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${sessionStorage.getItem('xv.ui.token')}` },
      body: form,
    });
    if (!response.ok) throw new Error(`upload failed: ${response.status}`);
  }, { name, destination, body });
}

for (const viewport of [
  { width: 768, height: 700 },
  { width: 390, height: 844 },
]) {
  test(`stacked rows preserve full identifiers without overflow at ${viewport.width}px`, async ({ page, baseURL }) => {
    await page.setViewportSize(viewport);
    await page.goto(baseURL);
    await createLongSecret(page);
    await createSecret(page, 'zz-next-secret');

    const list = page.locator('#secrets-stacked');
    await expect(list).toBeVisible();
    await expect(page.locator('#secrets-table')).toBeHidden();
    const activation = page.getByRole('button', { name: `Edit secret ${longName}` });
    await expect(activation).toBeVisible();
    await expect(activation).toContainText(longName);
    await expect(activation).toHaveAccessibleDescription(new RegExp(`Folder: ${nestedFolder}`));
    await expect(activation.locator('..')).toContainText(nestedFolder);
    await expect.poll(() => activation.ariaSnapshot()).toContain(
      `button "Edit secret ${longName}"`,
    );
    await expect(page.locator('#secrets-stacked .stacked-identifier').filter({ hasText: longName })).toHaveText(longName);
    await expectNoHorizontalOverflow(page);
    await expectNoSeriousOrCriticalAxeViolations(page);

    await page.getByRole('button', { name: 'Select', exact: true }).click();
    const row = page.locator('#secrets-stacked .stacked-row').filter({ hasText: longName });
    const checkbox = page.getByRole('checkbox', { name: `Select secret ${longName}`, exact: true });
    await expect(checkbox).toBeVisible();
    await expect(checkbox).toHaveAccessibleDescription(new RegExp(`Folder: ${nestedFolder}`));
    await expect(row.locator('button, a, input')).toHaveCount(1);
    await expect(page.getByRole('button', { name: `Select secret ${longName}` })).toHaveCount(0);
    await checkbox.focus();
    await page.keyboard.press('Tab');
    await expect(row.locator(':focus')).toHaveCount(0);
    await page.keyboard.press('Shift+Tab');
    await expect(checkbox).toBeFocused();
    await page.keyboard.press('Space');
    await expect(checkbox).toBeChecked();
    await expect(checkbox).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('checkbox', { name: 'Select secret zz-next-secret', exact: true })).toBeFocused();
    await expectNoHorizontalOverflow(page);
  });
}

test('stacked file rows preserve full paths and expose exactly one action in either mode', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(baseURL);
  await uploadDirect(page);
  await uploadDirect(page, { name: 'zz-next-file.txt' });
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.getByRole('button', { name: 'Refresh files' }).click();

  const row = page.locator('#files-stacked .stacked-row').filter({ hasText: longFileName });
  const download = page.getByRole('link', { name: `Download file ${longFileName}`, exact: true });
  await expect(download).toBeVisible();
  await expect(download).toHaveAccessibleDescription(/Size: .* Type: text\/plain/);
  await expect(row.locator('button, a, input')).toHaveCount(1);
  await expect(page.locator('#files-stacked').getByRole('heading', { name: nestedFolder })).toHaveCount(1);
  await expect(page.locator('#files-stacked .stacked-identifier').filter({ hasText: longFileName })).toHaveText(longFileName);
  await expectNoHorizontalOverflow(page);

  await page.getByRole('button', { name: 'Select', exact: true }).click();
  const checkbox = page.getByRole('checkbox', { name: `Select file ${longFileName}`, exact: true });
  await expect(checkbox).toHaveAccessibleDescription(/Size: .* Type: text\/plain/);
  await expect(row.locator('button, a, input')).toHaveCount(1);
  await expect(page.getByRole('link', { name: `Download file ${longFileName}` })).toHaveCount(0);
  await checkbox.focus();
  await page.keyboard.press('Space');
  await expect(checkbox).toBeChecked();
  await expect(checkbox).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('checkbox', {
    name: `Select file ${nestedFolder}/zz-next-file.txt`,
    exact: true,
  })).toBeFocused();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await expectNoHorizontalOverflow(page);
});

test('stacked groups are semantic, unique, and independent of the active desktop sort', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 769, height: 700 });
  await page.goto(baseURL);
  for (const [name, folder] of [
    ['zeta', 'teams/beta'],
    ['second', 'teams/alpha'],
    ['first', 'teams/alpha'],
    ['again', 'teams/beta'],
  ]) {
    await page.locator('#new-secret').click();
    const form = page.locator('#secret-form');
    await form.locator('input[name="name"]').fill(name);
    await form.locator('textarea[name="value"]').fill('value');
    await form.locator('input[name="folder"]').fill(folder);
    await page.getByRole('button', { name: 'Create secret' }).click();
  }
  await page.locator('#secrets-table th[data-sort-key="name"] .sort-button').click();
  await page.setViewportSize({ width: 768, height: 700 });

  const groups = page.locator('#secrets-stacked .stacked-group');
  await expect(groups).toHaveCount(2);
  await expect(page.locator('#secrets-stacked').getByRole('heading', { name: 'teams/alpha' })).toHaveCount(1);
  await expect(page.locator('#secrets-stacked').getByRole('heading', { name: 'teams/beta' })).toHaveCount(1);
  await expect(groups.nth(0).locator('.stacked-identifier')).toHaveText(['second', 'first']);
  await expect(groups.nth(1).locator('.stacked-identifier')).toHaveText(['zeta', 'again']);
});

test('stacked mode preserves the empty-state action', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(baseURL);
  await expect(page.locator('#secrets-stacked')).toBeVisible();
  await expect(page.locator('#secrets-stacked').getByText('No secrets yet')).toBeVisible();
  await expect(page.locator('#secrets-stacked').getByRole('button', { name: 'New secret' })).toBeVisible();
});

test('stacked mode preserves loading, failed, and filtered states', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  let releaseSecrets;
  const secretsGate = new Promise((resolve) => { releaseSecrets = resolve; });
  await page.route('**/api/secrets?*', async (route) => {
    if (route.request().method() === 'GET') await secretsGate;
    await route.continue();
  });
  await page.goto(baseURL);
  await expect(page.locator('#secrets-stacked .skeleton-row')).toHaveCount(3);
  releaseSecrets();
  await expect(page.locator('#secrets-stacked').getByText('No secrets yet')).toBeVisible();

  await page.unroute('**/api/secrets?*');
  await createLongSecret(page);
  await page.getByLabel('Search secrets').fill('definitely-not-present');
  await expect(page.locator('#secrets-stacked').getByText('No matching secrets')).toBeVisible();
  await expectNoHorizontalOverflow(page);

  await page.route('**/api/secrets?*', async (route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({
        status: 500,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'xv-test-failed', message: 'forced failure' } }),
      });
      return;
    }
    await route.continue();
  });
  await page.reload();
  await expect(page.locator('#secrets-stacked').getByText('Couldn’t load secrets')).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('desktop table keeps sorting and resizing controls above the breakpoint', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 769, height: 700 });
  await page.goto(baseURL);
  await expect(page.locator('#secrets-table')).toBeVisible();
  await expect(page.locator('#secrets-stacked')).toBeHidden();
  await expect(page.locator('#secrets-table [role="separator"]')).toHaveCount(4);
  await expect(page.locator('#secrets-table .sort-button')).toHaveCount(5);
});

test('constrained desktop keeps table mode but replaces the folder rail with its filter sheet', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 820, height: 560 });
  await page.goto(baseURL);

  await expect(page.locator('#secrets-table')).toBeVisible();
  await expect(page.locator('#secrets-stacked')).toBeHidden();
  await expect(page.locator('#secrets-workspace .folder-sidebar')).toBeHidden();
  const filterFolders = page.getByRole('button', { name: 'Filter folders' });
  await expect(filterFolders).toBeVisible();
  await filterFolders.click();
  await expect(page.getByRole('dialog', { name: 'Filter secret folders' })).toBeVisible();
  await expectNoHorizontalOverflow(page);
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('runtime breakpoint changes replace the complete semantic surface and focus order', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 769, height: 700 });
  await page.goto(baseURL);
  await createLongSecret(page);
  await expect(page.locator('#secrets-table')).toBeVisible();
  await expect(page.locator('#secrets-table').getByRole('separator')).toHaveCount(4);
  const desktopAction = page.getByRole('button', { name: `Edit secret ${longName}`, exact: true });
  await desktopAction.focus();
  await expect(desktopAction).toBeFocused();

  await page.setViewportSize({ width: 768, height: 700 });
  await expect(page.locator('#secrets-table')).toBeHidden();
  await expect(page.locator('#secrets-table').getByRole('separator')).toHaveCount(0);
  await expect(page.locator('#secrets-table').getByRole('button')).toHaveCount(0);
  const stackedAction = page.getByRole('button', { name: `Edit secret ${longName}`, exact: true });
  await expect(stackedAction).toBeVisible();
  await expect(stackedAction).toBeFocused();

  await page.setViewportSize({ width: 769, height: 700 });
  await expect(page.locator('#secrets-table')).toBeVisible();
  await expect(page.locator('#secrets-stacked')).toBeHidden();
  await expect(page.locator('#secrets-table').getByRole('separator')).toHaveCount(4);
  await expect(desktopAction).toBeFocused();

  const sort = page.locator('#secrets-table th[data-sort-key="name"] .sort-button');
  await sort.focus();
  await page.setViewportSize({ width: 768, height: 700 });
  await expect(page.locator('#secrets-stacked')).toBeFocused();
  await page.setViewportSize({ width: 769, height: 700 });
  await expect(page.locator('#secrets-table')).toBeFocused();

  const resizer = page.locator('#secrets-table').getByRole('separator').first();
  await resizer.focus();
  await page.setViewportSize({ width: 768, height: 700 });
  await expect(page.locator('#secrets-stacked')).toBeFocused();
  expect(await page.evaluate(() => document.activeElement?.hidden)).toBeFalsy();

  await page.setViewportSize({ width: 769, height: 700 });
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  const tableCheckbox = page.locator('#secrets-table').getByRole('checkbox', {
    name: `Select secret ${longName}`,
    exact: true,
  });
  await tableCheckbox.focus();
  await page.setViewportSize({ width: 768, height: 700 });
  const stackedCheckbox = page.locator('#secrets-stacked').getByRole('checkbox', {
    name: `Select secret ${longName}`,
    exact: true,
  });
  await expect(stackedCheckbox).toBeFocused();
  await page.setViewportSize({ width: 769, height: 700 });
  await expect(tableCheckbox).toBeFocused();
  await tableCheckbox.click();
  await expect(tableCheckbox).toBeFocused();

  await page.locator('#refresh-secrets').focus();
  await tableCheckbox.evaluate((control) => {
    control.checked = !control.checked;
    control.dispatchEvent(new Event('change', { bubbles: true }));
  });
  await expect(page.locator('#refresh-secrets')).toBeFocused();
  await page.setViewportSize({ width: 768, height: 700 });
  await expect(page.locator('#refresh-secrets')).toBeFocused();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('sheets fill the viewport below 544px', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(baseURL);
  await page.locator('#new-secret').click();
  const box = await page.getByRole('dialog', { name: 'New secret' }).boundingBox();
  expect(box).toEqual(expect.objectContaining({ x: 0, y: 0, width: 390, height: 844 }));
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.locator('#secrets-folder-filter-open').click();
  const folderBox = await page.getByRole('dialog', { name: 'Filter secret folders' }).boundingBox();
  expect(folderBox).toEqual(expect.objectContaining({ x: 0, y: 0, width: 390, height: 844 }));
});

test('desktop permits exercising the approved responsive breakpoints', async () => {
  const config = JSON.parse(await readFile(
    path.join(workspace, 'desktop/src-tauri/tauri.conf.json'),
    'utf8',
  ));
  expect(config.app.windows[0].minWidth).toBe(768);
});
