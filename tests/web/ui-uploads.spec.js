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
    rowFileReferences: 0,
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
  let releaseEvidence;
  const evidenceGate = new Promise((resolve) => { releaseEvidence = resolve; });
  let holdEvidence = false;
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === 'GET' && url.pathname === '/api/files' && holdEvidence) {
      await evidenceGate;
      await route.continue();
      return;
    }
    if (route.request().method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
    holdEvidence = true;
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

  await expect(page.getByText('Reconciling server evidence…')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Retry cancel-evidence.txt' })).toHaveCount(0);
  await expect(page.locator('#upload-summary')).toBeHidden();
  expect(await page.evaluate(() => window.__xvTestStoreSnapshot().scopedMutationPending)).toBe(true);
  await page.locator('#retry-uploads').dispatchEvent('click');
  await expect(page.getByText('Reconciling server evidence…')).toBeVisible();
  expect(pageErrors).toEqual([]);
  releaseEvidence();

  await expect(page.locator('#upload-summary')).toBeVisible();
  await expect(page.locator('#upload-summary-items li')).toHaveCount(1);
  await expect(page.locator('#upload-summary')).toContainText(/cancel-evidence\.txt: (Cancelled|Completion could not be confirmed)/);
  await expect(page.locator('#upload-summary')).toContainText(/metadata|destination file|unconfirmed/i);
  await expect(page.getByRole('button', { name: 'Retry unfinished' })).toBeVisible();
  await page.getByRole('button', { name: 'Retry unfinished' }).click();
  await expect(page.getByRole('link', { name: 'cancel-evidence.txt' })).toBeVisible();
  await expect(page.locator('#upload-summary')).toContainText('cancel-evidence.txt: Completed');
  await expectNoSeriousOrCriticalAxeViolations(page);
});

test('mismatched 2xx confirmation reconciles instead of completing', async ({ page, baseURL }) => {
  let releaseEvidence;
  const evidenceGate = new Promise((resolve) => { releaseEvidence = resolve; });
  let holdEvidence = false;
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === 'GET' && url.pathname === '/api/files' && holdEvidence) {
      await evidenceGate;
      await route.continue();
      return;
    }
    if (route.request().method() === 'POST' && url.pathname === '/api/files') {
      holdEvidence = true;
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ name: 'wrong-target.txt' }),
      });
      return;
    }
    await route.continue();
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'expected-target.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('target'),
  });
  await expect(page.getByText('Reconciling server evidence…')).toBeVisible();
  await expect(page.locator('#upload-summary')).toBeHidden();
  releaseEvidence();
  await expect(page.locator('#upload-summary')).toContainText(
    /expected-target\.txt: (Cancelled|Completion could not be confirmed)/,
  );
  await expect(page.locator('#upload-summary')).not.toContainText('Completed');
});

test('two uncertain items share one evidence refresh and retain pending ownership until it settles', async ({ page, baseURL }) => {
  let releaseEvidence;
  const evidenceGate = new Promise((resolve) => { releaseEvidence = resolve; });
  let uploadRequests = 0;
  let evidenceRefreshes = 0;
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === 'POST' && url.pathname === '/api/files') {
      uploadRequests++;
      await new Promise((resolve) => setTimeout(resolve, 500));
      await route.continue().catch(() => {});
      return;
    }
    if (route.request().method() === 'GET' && url.pathname === '/api/files' && uploadRequests === 2) {
      evidenceRefreshes++;
      await evidenceGate;
    }
    await route.continue();
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles([
    { name: 'uncertain-one.txt', mimeType: 'text/plain', buffer: Buffer.alloc(64 * 1024, 1) },
    { name: 'uncertain-two.txt', mimeType: 'text/plain', buffer: Buffer.alloc(64 * 1024, 2) },
  ]);
  await expect.poll(() => uploadRequests).toBe(2);
  await page.getByRole('button', { name: 'Cancel uncertain-one.txt' }).click();
  await page.getByRole('button', { name: 'Cancel uncertain-two.txt' }).click();
  await expect(page.getByText('Reconciling server evidence…')).toHaveCount(2);
  expect(await page.evaluate(() => window.__xvTestStoreSnapshot().scopedMutationPending)).toBe(true);
  await expect.poll(() => evidenceRefreshes).toBe(1);
  releaseEvidence();
  await expect(page.locator('#upload-summary-items li')).toHaveCount(2);
  expect(evidenceRefreshes).toBe(1);
});

test('manual refresh abort makes evidence unavailable rather than falsely missing', async ({ page, baseURL }) => {
  let uploadSeen = false;
  let releaseEvidence;
  const evidenceGate = new Promise((resolve) => { releaseEvidence = resolve; });
  let evidenceRefreshes = 0;
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === 'POST' && url.pathname === '/api/files') {
      uploadSeen = true;
      await new Promise((resolve) => setTimeout(resolve, 500));
      await route.continue().catch(() => {});
      return;
    }
    if (route.request().method() === 'GET' && url.pathname === '/api/files' && uploadSeen) {
      evidenceRefreshes++;
      if (evidenceRefreshes === 1) await evidenceGate;
    }
    await route.continue();
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'manual-refresh-race.txt',
    mimeType: 'text/plain',
    buffer: Buffer.alloc(64 * 1024, 4),
  });
  await expect.poll(() => uploadSeen).toBe(true);
  await page.getByRole('button', { name: 'Cancel manual-refresh-race.txt' }).click();
  await expect(page.getByText('Reconciling server evidence…')).toBeVisible();
  await page.getByRole('button', { name: 'Refresh files' }).dispatchEvent('click');
  releaseEvidence();
  await expect(page.locator('#upload-summary')).toContainText(
    'manual-refresh-race.txt: Completion could not be confirmed',
  );
  await expect(page.locator('#upload-summary')).not.toContainText('Cancelled');
});

test('sibling progress preserves focused conflict controls and apply-all state', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'persistent-conflict.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('seed'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === 'POST' && url.pathname === '/api/files') {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    await route.continue();
  });
  await page.locator('#file-input').setInputFiles([
    { name: 'persistent-conflict.txt', mimeType: 'text/plain', buffer: Buffer.from('again') },
    { name: 'progress-sibling.txt', mimeType: 'text/plain', buffer: Buffer.alloc(128 * 1024, 3) },
  ]);
  const conflict = page.locator('.upload-item').filter({ hasText: 'persistent-conflict.txt' });
  const applyAll = conflict.getByLabel('Apply to all remaining conflicts');
  const rename = conflict.getByRole('button', { name: 'Rename' });
  await applyAll.check();
  await rename.focus();
  await expect(page.getByText(/Uploading…|Finishing…/).first()).toBeVisible();
  await expect(applyAll).toBeChecked();
  await expect(rename).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('link', { name: 'persistent-conflict (2).txt' })).toBeVisible();
});

test('apply-all rename persists for a delayed transfer-time conflict using its own safe suggestion', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'policy-anchor.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('seed'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  let releaseLateConflict;
  const lateConflictGate = new Promise((resolve) => { releaseLateConflict = resolve; });
  let releaseLateRetry;
  const lateRetryGate = new Promise((resolve) => { releaseLateRetry = resolve; });
  let resolveLateRetry;
  const lateRetrySeen = new Promise((resolve) => { resolveLateRetry = resolve; });
  let delayedAttempts = 0;
  await page.route('**/api/files?*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (request.method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
    const body = request.postDataBuffer()?.toString() || '';
    if (!body.includes('late-policy.txt')) {
      await route.continue();
      return;
    }
    delayedAttempts++;
    if (delayedAttempts === 1) {
      await lateConflictGate;
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-file-conflict',
            message: 'Destination changed',
            details: { suggested_name: 'late-policy (2).txt' },
          },
        }),
      });
      return;
    }
    resolveLateRetry();
    await lateRetryGate;
    await route.continue();
  });
  await page.locator('#file-input').setInputFiles([
    { name: 'policy-anchor.txt', mimeType: 'text/plain', buffer: Buffer.from('again') },
    { name: 'late-policy.txt', mimeType: 'text/plain', buffer: Buffer.from('late') },
  ]);
  const anchor = page.locator('.upload-item').filter({ hasText: 'policy-anchor.txt' });
  await anchor.getByLabel('Apply to all remaining conflicts').check();
  await anchor.getByRole('button', { name: 'Rename' }).click();
  releaseLateConflict();
  await lateRetrySeen;
  await expect.poll(() => page.evaluate(() => window.__xvUploadDebug().controllers)).toBe(1);
  releaseLateRetry();
  await expect(page.getByRole('link', { name: 'late-policy (2).txt' })).toBeVisible();
  await expect(page.getByText('Needs a conflict decision')).toHaveCount(0);
  expect(delayedAttempts).toBe(2);
});

test('apply-all skip reports an upload when a delayed conflict disappeared', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'skip-anchor.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('seed'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  let releaseLateConflict;
  const lateConflictGate = new Promise((resolve) => { releaseLateConflict = resolve; });
  let delayedAttempts = 0;
  await page.route('**/api/files?*', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const body = request.postDataBuffer()?.toString() || '';
    if (request.method() !== 'POST' || url.pathname !== '/api/files' || !body.includes('late-skip.txt')) {
      await route.continue();
      return;
    }
    delayedAttempts++;
    if (delayedAttempts === 1) {
      await lateConflictGate;
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            code: 'xv-file-conflict',
            message: 'Destination changed',
            details: { suggested_name: 'late-skip (2).txt' },
          },
        }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ name: 'late-skip.txt', size: 4 }),
    });
  });
  await page.locator('#file-input').setInputFiles([
    { name: 'skip-anchor.txt', mimeType: 'text/plain', buffer: Buffer.from('again') },
    { name: 'late-skip.txt', mimeType: 'text/plain', buffer: Buffer.from('late') },
  ]);
  const anchor = page.locator('.upload-item').filter({ hasText: 'skip-anchor.txt' });
  await anchor.getByLabel('Apply to all remaining conflicts').check();
  await anchor.getByRole('button', { name: 'Skip', exact: true }).click();
  releaseLateConflict();
  await expect(page.locator('#upload-summary')).toContainText(
    'late-skip.txt: Completed — Uploaded because the destination no longer existed.',
  );
  expect(delayedAttempts).toBe(2);
});

test('terminal batch requires explicit dismissal before a new selection', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'first-owned.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('first'),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();

  await page.locator('#file-input').setInputFiles({
    name: 'must-wait-for-dismissal.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('blocked'),
  });
  await expect(page.locator('#upload-new-batch-message')).toBeVisible();
  await expect(page.locator('#upload-new-batch-message')).toBeFocused();
  await expect(page.locator('.upload-item')).toHaveCount(1);
  await expect(page.locator('#upload-queue')).not.toContainText('must-wait-for-dismissal.txt');

  await page.getByRole('button', { name: 'Dismiss summary' }).click();
  await page.locator('#file-input').setInputFiles({
    name: 'after-dismissal.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('allowed'),
  });
  await expect(page.locator('#upload-summary')).toContainText('after-dismissal.txt: Completed');
});

test('completed and skipped rows retain no File bytes before explicit dismissal', async ({ page, baseURL }) => {
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'released-bytes.txt',
    mimeType: 'text/plain',
    buffer: Buffer.alloc(128 * 1024, 9),
  });
  await expect(page.locator('#upload-summary')).toBeVisible();
  expect(await page.evaluate(() => window.__xvUploadDebug())).toMatchObject({
    fileReferences: 0,
    rowFileReferences: 0,
  });
  await page.getByRole('button', { name: 'Dismiss summary' }).click();

  await page.locator('#file-input').setInputFiles({
    name: 'released-bytes.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('conflict'),
  });
  await page.getByRole('button', { name: 'Skip' }).click();
  await expect(page.locator('#upload-summary')).toContainText('released-bytes.txt: Completed');
  expect(await page.evaluate(() => window.__xvUploadDebug())).toMatchObject({
    fileReferences: 0,
    rowFileReferences: 0,
  });
});

test('a dismissed batch callback cannot mutate a newer batch in the same scope', async ({ page, baseURL }) => {
  let releaseOldUpload;
  const oldUploadGate = new Promise((resolve) => { releaseOldUpload = resolve; });
  let resolveOldRequest;
  const oldRequestSeen = new Promise((resolve) => { resolveOldRequest = resolve; });
  let uploadCount = 0;
  const pageErrors = [];
  page.on('pageerror', (error) => pageErrors.push(error.message));
  await page.route('**/api/files?*', async (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() !== 'POST' || url.pathname !== '/api/files') {
      await route.continue();
      return;
    }
    uploadCount++;
    if (uploadCount === 1) {
      resolveOldRequest();
      await oldUploadGate;
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ name: 'old-delayed.txt' }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ name: 'new-current.txt' }),
    });
  });
  await openFiles(page, baseURL);
  await page.locator('#file-input').setInputFiles({
    name: 'old-delayed.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('old'),
  });
  await oldRequestSeen;
  await page.locator('#dismiss-upload-summary').dispatchEvent('click');
  await page.locator('#file-input').setInputFiles({
    name: 'new-current.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('new'),
  });
  await expect(page.locator('#upload-summary')).toContainText('new-current.txt: Completed');
  releaseOldUpload();
  await expect.poll(() => pageErrors).toEqual([]);
  await expect(page.locator('#upload-summary-items li')).toHaveCount(1);
  await expect(page.locator('#upload-summary')).toContainText('new-current.txt: Completed');
  await expect(page.locator('#upload-summary')).not.toContainText('old-delayed.txt');
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
    rowFileReferences: 0,
    names: [],
    controllers: 0,
    operationIds: 0,
  });
});
