import { test, expect } from './fixtures.js';

test('killing xv ui tells the open tab instead of leaving it looking healthy', async ({ page, baseURL, stopServer }) => {
  await page.goto(baseURL);
  await expect(page.locator('#context-connection')).toHaveAttribute('data-state', 'connected');
  await expect(page.locator('#connection-banner')).toBeHidden();

  await stopServer();

  const banner = page.locator('#connection-banner');
  await expect(banner).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('#connection-banner-title')).toHaveText('Disconnected');
  await expect(banner).toContainText('restart it with xv ui');
  // The rail must not keep claiming a healthy backend once the server it
  // learned that from is gone.
  await expect(page.locator('#context-connection')).toHaveAttribute('data-state', 'unavailable');
});

test('a restarted server with a new token asks for the new session link', async ({ page, baseURL }) => {
  await page.goto(baseURL);
  await expect(page.locator('#connection-banner')).toBeHidden();

  // Stand in for "same port, new process, new token": the socket answers, the
  // token does not match.
  await page.route('**/api/health', (route) => route.fulfill({
    status: 401,
    contentType: 'application/json',
    body: JSON.stringify({ error: { code: 'xv-unauthorized', message: 'missing or invalid token', hint: '' } }),
  }));

  await expect(page.locator('#connection-banner-title')).toHaveText('Session link required', { timeout: 30_000 });
  await expect(page.locator('#connection-banner')).toContainText('Reopen the URL');
});

test('a transient probe failure does not flash the banner', async ({ page, baseURL }) => {
  // Routed before navigating: the monitor probes once the moment it mounts,
  // and that probe is the only one inside the confirm window. Installing the
  // route after goto() would miss it and leave nothing to intercept until the
  // next full interval, so the test would pass without ever failing a probe.
  let probes = 0;
  await page.route('**/api/health', async (route) => {
    probes++;
    if (probes === 1) {
      await route.abort('connectionfailed');
      return;
    }
    await route.continue();
  });

  await page.goto(baseURL);
  await expect.poll(() => probes, { timeout: 15_000 }).toBeGreaterThanOrEqual(1);

  // Sample continuously rather than once at the end. A banner raised on the
  // first failure heals on the next successful probe, so a single late
  // assertion sees "hidden" either way and would pass against a debounce that
  // does not debounce.
  const banner = page.locator('#connection-banner');
  const deadline = Date.now() + 4_000;
  while (Date.now() < deadline) {
    await expect(banner).toBeHidden();
    await page.waitForTimeout(100);
  }

  // The failed probe schedules a fast recheck rather than waiting out the full
  // interval, so a second probe inside this window proves the retry path ran.
  expect(probes).toBeGreaterThanOrEqual(2);
});
