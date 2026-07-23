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
