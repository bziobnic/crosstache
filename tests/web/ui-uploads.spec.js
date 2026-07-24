import {
  test,
  expect,
  expectNoSeriousOrCriticalAxeViolations,
} from './fixtures.js';

async function openFiles(page, baseURL) {
  await page.goto(baseURL);
  await page.getByRole('tab', { name: 'Files' }).click();
  await expect(page.locator('#upload-concurrency')).toHaveText('up to 3 uploads at a time');
  await expect(page.getByLabel('Upload destination')).toHaveValue('');
}

test('managed queue bounds transfers, distinguishes Finishing, and keeps a named summary', async ({ page, baseURL }) => {
  let active = 0;
  let maxActive = 0;
  const uploadUrls = [];
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
    uploadUrls.push(route.request().url());
    active++;
    maxActive = Math.max(maxActive, active);
    await new Promise((resolve) => setTimeout(resolve, 250));
    active--;
    await route.continue();
  });
  await openFiles(page, baseURL);
  await page.evaluate(() => {
    window.__sawUploadFinishing = false;
    new MutationObserver(() => {
      if ([...document.querySelectorAll('.upload-item-status')]
        .some((element) => element.textContent === 'Finishing…')) {
        window.__sawUploadFinishing = true;
      }
    }).observe(document.querySelector('#upload-queue-items'), {
      childList: true,
      subtree: true,
      characterData: true,
    });
  });

  await page.locator('#file-input').setInputFiles(
    Array.from({ length: 5 }, (_, index) => ({
      name: `bounded-${index + 1}.txt`,
      mimeType: 'text/plain',
      buffer: Buffer.from(`file ${index + 1}`),
    })),
  );

  await expect.poll(() => page.evaluate(() => window.__sawUploadFinishing)).toBe(true);
  await expect(page.locator('#upload-summary')).toBeVisible();
  await expect(page.locator('#upload-summary-items li')).toHaveCount(5);
  for (let index = 1; index <= 5; index++) {
    await expect(page.locator('#upload-summary')).toContainText(`bounded-${index}.txt: Completed`);
  }
  expect(maxActive).toBeLessThanOrEqual(3);
  expect(uploadUrls).toHaveLength(5);
  expect(uploadUrls.every((url) => new URL(url).searchParams.has('destination'))).toBe(true);
  await expectNoSeriousOrCriticalAxeViolations(page);

  await page.getByRole('button', { name: 'Dismiss summary' }).click();
  await expect(page.locator('#upload-summary')).toBeHidden();
  await expect(page.locator('#upload-queue')).toBeHidden();
  expect(await page.evaluate(() => window.__xvUploadDebug())).toEqual({
    hasBatch: false,
    fileReferences: 0,
    names: [],
    controllers: 0,
    operationIds: 0,
  });
  expect(await page.evaluate(() => Object.keys(window.__xvTestStoreSnapshot().operations || {})
    .filter((id) => id.startsWith('upload-')))).toEqual([]);
});

test('conflicts require an explicit per-item or apply-all decision and Replace is never implicit', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  const files = [
    { name: 'conflict-one.txt', mimeType: 'text/plain', buffer: Buffer.from('one') },
    { name: 'conflict-two.txt', mimeType: 'text/plain', buffer: Buffer.from('two') },
  ];
  await page.locator('#file-input').setInputFiles(files);
  await expect(page.locator('#upload-summary')).toBeVisible();
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  await page.locator('#file-input').setInputFiles(files);
  await expect(page.getByText('Needs a conflict decision')).toHaveCount(2);
  const first = page.locator('.upload-item').filter({ hasText: 'conflict-one.txt' });
  await first.getByLabel('Apply to all remaining conflicts').check();
  await first.getByRole('button', { name: 'Rename' }).click();

  await expect(page.locator('#upload-summary')).toBeVisible();
  await expect(page.getByRole('link', { name: 'conflict-one (2).txt' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'conflict-two (2).txt' })).toBeVisible();
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('cancellation refreshes metadata evidence and remains retryable without guessing', async ({ page, baseURL }) => {
  let intercepted;
  const requestSeen = new Promise((resolve) => { intercepted = resolve; });
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
    intercepted();
    await new Promise((resolve) => setTimeout(resolve, 500));
    await route.continue().catch(() => {});
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'cancel-evidence.txt',
    mimeType: 'text/plain',
    buffer: Buffer.alloc(64 * 1024, 7),
  });
  await requestSeen;
  await page.getByRole('button', { name: 'Cancel cancel-evidence.txt' }).click();

  await expect(page.locator('#upload-summary')).toBeVisible();
  await expect(page.locator('#upload-summary')).toContainText(/cancel-evidence\.txt: (Cancelled|Completion could not be confirmed)/);
  await expect(page.locator('#upload-summary')).toContainText(/metadata|destination file|unconfirmed/i);
  await expect(page.getByRole('button', { name: 'Retry unfinished' })).toBeVisible();
  await page.getByRole('button', { name: 'Retry unfinished' }).click();
  await expect(page.getByRole('link', { name: 'cancel-evidence.txt' })).toBeVisible();
  await expect(page.locator('#upload-summary')).toContainText('cancel-evidence.txt: Completed');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('selected destination is used for ready and replace uploads', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.evaluate(async () => {
    const context = window.__xvTestStoreSnapshot().context;
    const query = new URLSearchParams({
      alias: context.workspace.alias,
      backend: context.backend,
      vault: context.vault,
      policy: 'replace',
      destination: 'docs',
    });
    const form = new FormData();
    form.append('file', new File(['seed'], 'seed.txt', { type: 'text/plain' }));
    await fetch(`/api/files?${query}`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${sessionStorage.getItem('xv.ui.token')}` },
      body: form,
    });
  });
  await page.getByRole('button', { name: 'Refresh files' }).click();
  await expect(page.getByLabel('Upload destination').getByRole('option', { name: 'docs' })).toBeAttached();
  await page.getByLabel('Upload destination').selectOption('docs');
  await page.locator('#file-input').setInputFiles({
    name: 'placed.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('first'),
  });
  await expect(page.getByRole('link', { name: 'docs/placed.txt' })).toBeVisible();
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  await page.locator('#file-input').setInputFiles({
    name: 'placed.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('replacement'),
  });
  const row = page.locator('.upload-item').filter({ hasText: 'docs/placed.txt' });
  await row.getByRole('button', { name: 'Replace' }).click();
  await expect(page.locator('#upload-summary')).toContainText('docs/placed.txt: Completed');
});

test('per-row preflight cancel and retry are isolated and active work blocks tab navigation', async ({ page, baseURL }) => {
  let releasePreflight;
  const gate = new Promise((resolve) => { releasePreflight = resolve; });
  let firstPreflight = true;
  await page.route('**/api/files/preflight?*', async (route) => {
    if (firstPreflight) {
      firstPreflight = false;
      await gate;
    }
    await route.continue();
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles([
    { name: 'cancel-only-this.txt', mimeType: 'text/plain', buffer: Buffer.from('cancel') },
    { name: 'keep-this.txt', mimeType: 'text/plain', buffer: Buffer.from('keep') },
  ]);
  await expect(page.getByText('Checking destination…')).toHaveCount(2);
  await page.getByRole('button', { name: 'Cancel cancel-only-this.txt' }).click();
  await expect(page.locator('#progress')).toBeVisible();
  expect(await page.evaluate(() => window.__xvTestStoreSnapshot().scopedMutationPending)).toBe(true);
  const secretsTab = page.getByRole('tab', { name: 'Secrets' });
  await secretsTab.focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true');
  expect(await page.evaluate(() => {
    const event = new Event('beforeunload', { cancelable: true });
    return window.dispatchEvent(event);
  })).toBe(false);
  releasePreflight();

  await expect(page.getByRole('link', { name: 'keep-this.txt' })).toBeVisible();
  await expect(page.locator('#upload-summary')).toContainText('cancel-only-this.txt: Cancelled');
  await expect(page.locator('#upload-summary')).toContainText('keep-this.txt: Completed');
  await page.getByRole('button', { name: 'Retry cancel-only-this.txt' }).click();
  await expect(page.getByRole('link', { name: 'cancel-only-this.txt' })).toBeVisible();
});

test('malformed preflight settles safely with item retries and no pending owner', async ({ page, baseURL }) => {
  await page.route('**/api/files/preflight?*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        results: [
          { client_id: 'file-1', status: 'ready' },
          { client_id: 'file-1', status: 'ready' },
        ],
      }),
    });
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles([
    { name: 'malformed-a.txt', mimeType: 'text/plain', buffer: Buffer.from('a') },
    { name: 'malformed-b.txt', mimeType: 'text/plain', buffer: Buffer.from('b') },
  ]);
  await expect(page.getByText('Failed', { exact: true })).toHaveCount(2);
  await expect(page.getByRole('button', { name: 'Retry malformed-a.txt' })).toBeVisible();
  expect(await page.evaluate(() => window.__xvTestStoreSnapshot().scopedMutationPending)).toBe(false);
});

test('mobile queue keeps statuses and exact-item actions visible without horizontal overflow', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'a-very-long-mobile-upload-name-that-must-wrap-cleanly.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('mobile'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('failed context transition preserves owned summary while a committed transition scrubs it', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'owned-until-context-commit.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('owned'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();

  let failOnce = true;
  await page.route('**/api/workspaces/activate', async (route) => {
    if (failOnce) {
      failOnce = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'xv-network', message: 'Unavailable' } }),
      });
      return;
    }
    await route.continue();
  });
  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#upload-summary')).toBeVisible();
  expect((await page.evaluate(() => window.__xvUploadDebug())).hasBatch).toBe(true);

  await page.locator('#workspace-select').selectOption('sandbox');
  await expect(page.locator('#upload-summary')).toBeHidden();
  expect(await page.evaluate(() => window.__xvUploadDebug())).toEqual({
    hasBatch: false,
    fileReferences: 0,
    names: [],
    controllers: 0,
    operationIds: 0,
  });
});
