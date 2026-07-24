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
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
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
  await expectNoSeriousOrCriticalAxeViolations(page);

  await page.getByRole('button', { name: 'Dismiss summary' }).click();
  await expect(page.locator('#upload-summary')).toBeHidden();
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
