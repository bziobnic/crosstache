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

function grid(page, surface = 'secrets') {
  return page.locator(`#${surface}-table tbody`);
}

function folderRow(page, path, surface = 'secrets') {
  return grid(page, surface).locator(`tr[data-tree-path="${path}"]`);
}

function itemRow(page, label, surface = 'secrets') {
  return grid(page, surface).locator(`tr[aria-label="${label}"]`);
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

test('the tree grid nests items under folders, drives them by keyboard, and restores scoped expansion', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: primarySecrets,
      sandbox: [{ name: 'sandbox-secret', folder: 'other/nested' }],
    },
  });
  await page.goto(baseURL);

  const apps = folderRow(page, 'apps');
  await expect(apps).toHaveAttribute('aria-expanded', 'false');
  await expect(apps).toHaveAttribute('aria-level', '1');
  await expect(itemRow(page, 'Secret prod-secret')).toHaveCount(0);

  await apps.focus();
  await page.keyboard.press('ArrowRight');
  await expect(apps).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('ArrowRight');
  await expect(folderRow(page, 'apps/prod')).toBeFocused();
  await page.keyboard.press('ArrowRight');
  await expect(folderRow(page, 'apps/prod')).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('ArrowDown');
  await expect(itemRow(page, 'Secret prod-secret')).toBeFocused();
  await expect(itemRow(page, 'Secret prod-secret')).toHaveAttribute('aria-level', '3');
  await page.keyboard.press('ArrowLeft');
  await expect(folderRow(page, 'apps/prod')).toBeFocused();

  await expect(page.getByRole('button', { name: 'Edit secret prod-secret' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit secret loose-0' })).toBeVisible();
  await expect(page.locator('#secret-list-summary')).toContainText('51 secrets across 3 folders');
  await expect(page.locator('#secrets-expand-all')).toBeVisible();
  await expect(page.locator('#secrets-collapse-all')).toBeVisible();

  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#context-line')).toContainText('local / sandbox');
  await expect(folderRow(page, 'other')).toHaveAttribute('aria-expanded', 'true');
  await expect(folderRow(page, 'apps')).toHaveCount(0);

  await page.locator('#workspace-select').selectOption('playwright');
  await expect(page.locator('#context-line')).toContainText('local / playwright');
  await expect(folderRow(page, 'apps')).toHaveAttribute('aria-expanded', 'true');
  await expect(folderRow(page, 'apps/prod')).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('a flat vault of folders still expands and collapses from the row and the toolbar', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [
        { name: 'a', folder: 'prod' },
        { name: 'b', folder: 'dev' },
        { name: 'c', folder: null },
      ],
    },
  });
  await page.goto(baseURL);

  // Regression: with only single-segment folders the previous sidebar tree had
  // no expandable node, so every expand/collapse control was a silent no-op.
  const prod = folderRow(page, 'prod');
  await expect(prod).toHaveAttribute('aria-expanded', 'true');
  await expect(itemRow(page, 'Secret a')).toBeVisible();

  await prod.locator('.tree-disclosure').click();
  await expect(prod).toHaveAttribute('aria-expanded', 'false');
  await expect(itemRow(page, 'Secret a')).toHaveCount(0);

  await page.locator('#secrets-expand-all').click();
  await expect(prod).toHaveAttribute('aria-expanded', 'true');
  await expect(itemRow(page, 'Secret a')).toBeVisible();

  await page.locator('#secrets-collapse-all').click();
  await expect(folderRow(page, 'dev')).toHaveAttribute('aria-expanded', 'false');
  await expect(prod).toHaveAttribute('aria-expanded', 'false');
  await expect(itemRow(page, 'Secret c')).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('tri-state selection propagates down the tree and rolls up to ancestors', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [
        { name: 'alpha', folder: 'apps/prod' },
        { name: 'beta', folder: 'apps/prod' },
        { name: 'gamma', folder: 'apps/stage' },
      ],
    },
  });
  await page.goto(baseURL);
  await page.locator('#select-secrets').click();

  const checkbox = (row) => row.locator('.tree-checkbox');
  const mixed = (locator) => expect(locator).toHaveJSProperty('indeterminate', true);

  await checkbox(itemRow(page, 'Secret alpha')).check();
  await expect(page.locator('#secret-selection-count')).toHaveText('1 selected');
  await mixed(checkbox(folderRow(page, 'apps/prod')));
  await mixed(checkbox(folderRow(page, 'apps')));
  await expect(checkbox(folderRow(page, 'apps'))).toHaveAttribute('aria-checked', 'mixed');

  await checkbox(folderRow(page, 'apps/prod')).check();
  await expect(page.locator('#secret-selection-count')).toHaveText('2 selected');
  await expect(checkbox(folderRow(page, 'apps/prod'))).toBeChecked();
  await expect(itemRow(page, 'Secret beta')).toHaveAttribute('aria-selected', 'true');
  await mixed(checkbox(folderRow(page, 'apps')));

  await checkbox(folderRow(page, 'apps')).check();
  await expect(page.locator('#secret-selection-count')).toHaveText('3 selected');
  await expect(checkbox(folderRow(page, 'apps/stage'))).toBeChecked();
  await expect(page.locator('#bulk-delete-secrets')).toBeEnabled();
  await expect(page.locator('#bulk-move-secrets')).toBeEnabled();
  await expect(page.locator('#select-all-secrets')).toBeChecked();

  await checkbox(folderRow(page, 'apps')).uncheck();
  await expect(page.locator('#secret-selection-count')).toHaveText('0 selected');
  await expect(page.locator('#bulk-delete-secrets')).toBeDisabled();

  await itemRow(page, 'Secret gamma').focus();
  await page.keyboard.press('Space');
  await expect(page.locator('#secret-selection-count')).toHaveText('1 selected');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('typed folder identities stay unique and opaque while rerenders keep one focused row', async ({ page, baseURL }) => {
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

  const rows = grid(page).locator('tr[data-tree-path]');
  const paths = await rows.evaluateAll((nodes) => nodes.map((node) => node.dataset.treePath));
  expect(new Set(paths).size).toBe(paths.length);
  await expect(folderRow(page, '__all__')).toBeVisible();
  await expect(folderRow(page, '__unfiled__')).toBeVisible();
  await expect(itemRow(page, 'Secret unfiled')).toHaveAttribute('aria-level', '1');

  await folderRow(page, 'apps/prod').focus();
  await page.keyboard.press('ArrowLeft');
  await expect(folderRow(page, 'apps/prod')).toHaveAttribute('aria-expanded', 'false');
  await page.keyboard.press('ArrowLeft');
  await expect(folderRow(page, 'apps')).toBeFocused();
  await expect(grid(page).locator('[tabindex="0"]')).toHaveCount(1);

  await folderRow(page, 'apps').locator('.tree-disclosure').click();
  await expect(folderRow(page, 'apps')).toHaveAttribute('aria-expanded', 'false');
  const persisted = await page.evaluate(() => JSON.stringify(
    Object.entries(localStorage).filter(([key]) => key.startsWith('xv.ui.folder-expansion')),
  ));
  expect(persisted).toContain('.v5:');
  for (const source of ['local', 'playwright', 'apps', 'prod', '__all__', '__unfiled__']) {
    expect(persisted).not.toContain(source);
  }
  await expectNoSeriousOrCriticalAxeViolations(page);
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
  const primary = page.locator('#secrets-table .item-name-content strong').first();
  await expect(primary).toHaveCSS('white-space', 'normal');
  await expect(primary).toHaveCSS('overflow', 'visible');

  await page.setViewportSize({ width: 1180, height: 900 });
  await page.locator('#secrets-expand-all').click();
  const deepest = folderRow(page, deepFolder);
  await expect(deepest).toHaveAttribute('aria-level', '10');
  await expect(deepest.locator('td.tree-cell')).toHaveCSS('--tree-depth', '9');
  const eighthPadding = parseFloat(await folderRow(page, 'a/b/c/d/e/f/g/h').locator('td.tree-cell').evaluate(
    (element) => getComputedStyle(element).paddingInlineStart,
  ));
  const tenthPadding = parseFloat(await deepest.locator('td.tree-cell').evaluate(
    (element) => getComputedStyle(element).paddingInlineStart,
  ));
  expect(tenthPadding).toBeGreaterThan(eighthPadding);
});

test('the files surface uses the same tree grid with its own columns', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: { playwright: [] },
    filesByVault: {
      playwright: [
        { name: 'docs/prod/report.txt', size: 12, content_type: 'text/plain', last_modified: '2026-07-22T00:00:00Z' },
        { name: 'loose.txt', size: 4, content_type: 'text/plain', last_modified: '2026-07-22T00:00:00Z' },
      ],
    },
  });
  await page.goto(baseURL);
  await page.getByRole('tab', { name: 'Files' }).click();

  await expect(folderRow(page, 'docs', 'files')).toHaveAttribute('aria-expanded', 'true');
  await expect(folderRow(page, 'docs/prod', 'files')).toHaveAttribute('aria-level', '2');
  await expect(page.getByRole('link', { name: 'docs/prod/report.txt' })).toBeVisible();
  await expect(itemRow(page, 'File loose.txt', 'files')).toHaveAttribute('aria-level', '1');
  await expect(page.locator('#file-list-summary')).toContainText('2 files across 2 folders');

  await folderRow(page, 'docs', 'files').locator('.tree-disclosure').click();
  await expect(page.getByRole('link', { name: 'docs/prod/report.txt' })).toHaveCount(0);
  await expect(itemRow(page, 'File loose.txt', 'files')).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('a search reveals matches inside collapsed folders without persisting that expansion', async ({ page, baseURL }) => {
  await routeFolderFixtures(page, {
    secretsByVault: {
      playwright: [
        { name: 'needle-secret', folder: 'apps/prod' },
        ...Array.from({ length: 55 }, (_, index) => ({ name: `loose-${index}`, folder: null })),
      ],
    },
  });
  await page.goto(baseURL);

  await expect(folderRow(page, 'apps')).toHaveAttribute('aria-expanded', 'false');
  await page.locator('#search').fill('needle');
  await expect(itemRow(page, 'Secret needle-secret')).toBeVisible();
  await expect(folderRow(page, 'apps')).toHaveAttribute('aria-expanded', 'true');

  await page.locator('#secret-search-clear').click();
  await expect(folderRow(page, 'apps')).toHaveAttribute('aria-expanded', 'false');
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
  await expect(folderRow(page, 'current', 'files')).toBeVisible();

  await expect(folderRow(page, 'stale', 'files')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'current/path/new.txt' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'stale/path/old.txt' })).toHaveCount(0);
});
