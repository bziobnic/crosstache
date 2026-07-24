import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

const primarySecrets = [
  { name: 'prod-secret', folder: 'apps/prod' },
  { name: 'stage-secret', folder: 'apps/stage' },
  ...Array.from({ length: 49 }, (_, index) => ({ name: `loose-${index}`, folder: null })),
];

async function routeFolderFixtures(page, {
  secretsByVault = {},
  filesByVault = {},
  activationSecrets = true,
} = {}) {
  await page.route(/\/api\/secrets\?/, async (route) => {
    const vault = new URL(route.request().url()).searchParams.get('vault');
    await route.fulfill({ json: secretsByVault[vault] || [] });
  });
  await page.route(/\/api\/files\?/, async (route) => {
    const vault = new URL(route.request().url()).searchParams.get('vault');
    await route.fulfill({ json: filesByVault[vault] || [] });
  });
  if (activationSecrets) {
    await page.route('**/api/workspaces/activate', async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.secrets = secretsByVault[body.context?.vault] || [];
      await route.fulfill({ response, json: body });
    });
  }
}

function treeitem(tree, name) {
  return tree.getByRole('treeitem', { name });
}

test('no-results guidance names only searchable fields on secrets and files', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [{ name: 'visible-secret', note: 'not-searchable' }],
    },
    filesByVault: {
      playwright: [{ name: 'visible.pdf', size: 12, content_type: 'application/pdf', last_modified: '2026-07-22T00:00:00Z' }],
    },
  });
  await page.goto(baseURL);

  await page.locator('#search').fill('no-secret-match');
  await expect(page.locator('#secrets-table tbody')).toContainText(
    'Try a different name, folder, group, or record type.',
  );
  await expect(page.locator('#secrets-table tbody')).not.toContainText('note');

  await page.locator('#tab-files').click();
  await page.locator('#file-search').fill('no-file-match');
  await expect(page.locator('#files-table tbody')).toContainText(
    'Try a different name, folder, or type.',
  );
  await expect(page.locator('#files-table tbody')).not.toContainText('status');
});

test('desktop folder tree filters descendants, supports keyboard navigation, and restores scoped expansion', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: primarySecrets,
      sandbox: [{ name: 'sandbox-secret', folder: 'other/nested' }],
    },
  });
  await page.goto(baseURL);

  const tree = page.getByRole('tree', { name: 'Secret folders' });
  const apps = treeitem(tree, /^apps,/);
  await expect(apps).toHaveAttribute('aria-expanded', 'false');
  await expect(treeitem(tree, /^Unfiled,/)).toHaveAttribute('aria-selected', 'false');

  await apps.focus();
  await page.keyboard.press('ArrowRight');
  await expect(apps).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('ArrowRight');
  await expect(treeitem(tree, /^prod,/)).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(apps).toBeFocused();
  await page.keyboard.press('Enter');

  await expect(apps).toHaveAttribute('aria-selected', 'true');
  await expect(apps).toBeFocused();
  await expect(page.getByRole('button', { name: 'Edit secret prod-secret' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit secret stage-secret' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit secret loose-0' })).toHaveCount(0);
  await expect(page.locator('#secret-list-summary')).toContainText('2 of 51 secrets');
  await expect(page.locator('#secrets-folders-expand-all')).toBeVisible();
  await expect(page.locator('#secrets-folders-collapse-all')).toBeVisible();

  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  await expect(treeitem(tree, /^All items,/)).toHaveAttribute('aria-selected', 'true');
  await expect(treeitem(tree, /^other,/)).toHaveAttribute('aria-expanded', 'true');
  await expect(treeitem(tree, /^apps,/)).toHaveCount(0);

  await page.locator('#workspace-select').selectOption('playwright');
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await expect(treeitem(tree, /^All items,/)).toHaveAttribute('aria-selected', 'true');
  await expect(treeitem(tree, /^apps,/)).toHaveAttribute('aria-expanded', 'true');
  await expect(treeitem(tree, /^prod,/)).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('typed folder identities stay unique and opaque while rerenders keep one focused selection', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [
        { name: 'reserved-all', folder: '__all__' },
        { name: 'reserved-unfiled', folder: '__unfiled__' },
        { name: 'spaced', folder: ' apps / prod ' },
        { name: 'nested', folder: 'apps/prod' },
        { name: 'unfiled', folder: null },
      ],
    },
  });
  await page.goto(baseURL);

  const tree = page.getByRole('tree', { name: 'Secret folders' });
  const items = tree.getByRole('treeitem');
  const ids = await items.evaluateAll((nodes) => nodes.map((node) => node.dataset.folderId));
  expect(new Set(ids).size).toBe(ids.length);
  expect(ids.every((id) => /^folder-node-\d+$/.test(id))).toBe(true);
  await expect(treeitem(tree, /^__all__,/)).toBeVisible();
  await expect(treeitem(tree, /^__unfiled__,/)).toBeVisible();
  await expect(treeitem(tree, /^Unfiled,/)).toBeVisible();

  const apps = treeitem(tree, /^apps,/);
  await treeitem(tree, /^prod,/).focus();
  await page.keyboard.press('Enter');
  await expect(treeitem(tree, /^prod,/)).toBeFocused();
  await expect(tree.locator('[aria-selected="true"]')).toHaveCount(1);
  await expect(tree.locator('[tabindex="0"]')).toHaveCount(1);
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(apps).toBeFocused();
  await expect(apps).toHaveAttribute('aria-selected', 'true');
  await expect(tree.locator('[aria-selected="true"]')).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Edit secret nested' })).toBeVisible();

  const disclosure = apps.locator('.folder-tree-disclosure');
  await disclosure.click();
  await expect(apps).toHaveAttribute('aria-expanded', 'true');
  await expect(apps).toHaveAttribute('aria-selected', 'true');
  const persisted = await page.evaluate(() => JSON.stringify(
    Object.entries(localStorage).filter(([key]) => key.startsWith('xv.ui.folder-expansion')),
  ));
  for (const source of ['local', 'playwright', 'apps', 'prod', '__all__', '__unfiled__']) {
    expect(persisted).not.toContain(source);
  }
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('pointer disclosure toggles only its branch', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [{ name: 'prod-secret', folder: 'apps/prod' }],
    },
  });
  await page.goto(baseURL);
  const tree = page.getByRole('tree', { name: 'Secret folders' });
  const apps = treeitem(tree, /^apps,/);
  const all = treeitem(tree, /^All items,/);

  await expect(apps).toHaveAttribute('aria-expanded', 'true');
  await apps.locator('.folder-tree-disclosure').click();
  await expect(apps).toHaveAttribute('aria-expanded', 'false');
  await expect(all).toHaveAttribute('aria-selected', 'true');
  await apps.locator('.folder-tree-disclosure').dispatchEvent('click', { pointerType: 'touch' });
  await expect(apps).toHaveAttribute('aria-expanded', 'true');
  await expect(all).toHaveAttribute('aria-selected', 'true');
});

test('48rem layouts show full identifiers and ten-level trees keep increasing indentation', async ({ page, baseURL }) => {
  const deepFolder = 'a/b/c/d/e/f/g/h/i/j';
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [{
        name: 'a-very-long-primary-identifier-that-must-wrap-without-truncation',
        folder: deepFolder,
      }],
    },
  });
  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto(baseURL);
  const primary = page.locator('#secrets-table .item-name-content strong');
  await expect(primary).toHaveCSS('white-space', 'normal');
  await expect(primary).toHaveCSS('overflow', 'visible');
  await page.setViewportSize({ width: 600, height: 900 });
  await expect(primary).toHaveCSS('white-space', 'normal');
  await expect(primary).toHaveCSS('overflow', 'visible');

  await page.setViewportSize({ width: 1024, height: 900 });
  await expect(page.locator('#secrets-workspace .folder-sidebar')).toBeHidden();
  await expect(page.locator('#secrets-folder-filter-open')).toBeVisible();
  await page.setViewportSize({ width: 1025, height: 900 });
  await expect(page.locator('#secrets-workspace .folder-sidebar')).toBeVisible();
  await page.locator('#secrets-folders-expand-all').click();
  const tree = page.getByRole('tree', { name: 'Secret folders' });
  const deepest = treeitem(tree, /^j,/);
  await expect(deepest).toHaveAttribute('aria-level', '10');
  await expect(deepest).toHaveCSS('--folder-depth', '9');
  const eighthPadding = parseFloat(await treeitem(tree, /^h,/).evaluate(
    (element) => getComputedStyle(element).paddingInlineStart,
  ));
  const tenthPadding = parseFloat(await deepest.evaluate(
    (element) => getComputedStyle(element).paddingInlineStart,
  ));
  expect(tenthPadding).toBeGreaterThan(eighthPadding);
});

test('mobile filter sheets reuse folder models for secrets and files with visible controls', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [
        { name: 'prod-secret', folder: 'apps/prod' },
        { name: 'loose-secret', folder: null },
      ],
    },
    filesByVault: {
      playwright: [
        { name: 'docs/prod/report.txt', size: 12, content_type: 'text/plain', last_modified: '2026-07-22T00:00:00Z' },
        { name: 'loose.txt', size: 4, content_type: 'text/plain', last_modified: '2026-07-22T00:00:00Z' },
      ],
    },
  });
  await page.goto(baseURL);

  await expect(page.locator('#secrets-workspace .folder-sidebar')).toBeHidden();
  await expect(page.locator('#secrets-folder-filter-open')).toBeVisible();
  await expect(page.locator('#secrets-mobile-folders-expand-all')).toBeVisible();
  await expect(page.locator('#secrets-mobile-folders-collapse-all')).toBeVisible();
  await page.locator('#secrets-folder-filter-open').click();
  const secretSheet = page.getByRole('dialog', { name: 'Filter secret folders' });
  await expect(secretSheet).toBeVisible();
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expectNoSeriousOrCriticalAxeViolations(page);
  await treeitem(secretSheet.getByRole('tree', { name: 'Secret folder filter' }), /^prod,/).click();
  await expect(secretSheet).toBeHidden();
  await expect(page.getByRole('button', { name: 'Edit secret prod-secret' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit secret loose-secret' })).toHaveCount(0);

  await page.getByRole('tab', { name: 'Files' }).click();
  await expect(page.locator('#files-folder-filter-open')).toBeVisible();
  await expect(page.locator('#files-mobile-folders-expand-all')).toBeVisible();
  await expect(page.locator('#files-mobile-folders-collapse-all')).toBeVisible();
  await page.locator('#files-folder-filter-open').click();
  const fileSheet = page.getByRole('dialog', { name: 'Filter file folders' });
  const fileTree = fileSheet.getByRole('tree', { name: 'File folder filter' });
  await expect(treeitem(fileTree, /^docs,/)).toHaveAttribute('aria-expanded', 'true');
  await treeitem(fileTree, /^prod,/).click();
  await expect(fileSheet).toBeHidden();
  await expect(page.getByRole('link', { name: 'docs/prod/report.txt' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'loose.txt' })).toHaveCount(0);
  await expect(page.locator('#file-list-summary')).toContainText('1 of 2 files');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('an obsolete file response cannot publish its folders after a workspace switch', async ({ page, baseURL }) => {
  let releasePrimary;
  const primaryGate = new Promise((resolve) => { releasePrimary = resolve; });
  await page.route(/\/api\/files\?/, async (route) => {
    const vault = new URL(route.request().url()).searchParams.get('vault');
    if (vault === 'playwright') {
      await primaryGate;
      await route.fulfill({
        json: [{ name: 'stale/path/old.txt', size: 3, content_type: 'text/plain', last_modified: null }],
      });
      return;
    }
    await route.fulfill({
      json: [{ name: 'current/path/new.txt', size: 3, content_type: 'text/plain', last_modified: null }],
    });
  });
  await page.goto(baseURL);
  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  releasePrimary();
  await expect(page.locator('#progress')).toBeHidden();
  await page.getByRole('tab', { name: 'Files' }).click();
  const tree = page.getByRole('tree', { name: 'File folders' });
  await expect(treeitem(tree, /^current,/)).toBeVisible();

  await expect(treeitem(tree, /^stale,/)).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'current/path/new.txt' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'stale/path/old.txt' })).toHaveCount(0);
});
