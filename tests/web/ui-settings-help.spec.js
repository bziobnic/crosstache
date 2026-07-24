import { test, expect, expectNoSeriousOrCriticalAxeViolations } from './fixtures.js';

test('Settings applies live presentation preferences through the server owner', async ({ page, baseURL }) => {
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'no-preference' });
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');

  await page.locator('#settings-open').click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByRole('heading', { name: 'Settings' })).toBeVisible();
  await expect(settings.getByLabel('Theme')).toBeFocused();
  await expect(settings.getByLabel('Protected value timeout')).toHaveValue('30');
  await expect(settings.locator('#timeout-policy-copy')).toHaveText(
    'No application maximum is configured. A saved 0-second timeout hides protected values immediately.',
  );

  const darkSaved = page.waitForResponse((response) => (
    response.url().endsWith('/api/preferences') && response.request().method() === 'PUT'
  ));
  await settings.getByLabel('Theme').selectOption('dark');
  await darkSaved;
  const themeSaved = page.waitForResponse((response) => (
    response.url().endsWith('/api/preferences') && response.request().method() === 'PUT'
  ));
  await settings.getByLabel('Theme').selectOption('system');
  await themeSaved;
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'system');
  await expect(page.locator('html')).toHaveAttribute('data-effective-theme', 'dark');
  await page.screenshot({
    path: '/tmp/crosstache-task5-settings-desktop.png',
    fullPage: true,
    animations: 'disabled',
  });
  await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'no-preference' });
  await expect(page.locator('html')).toHaveAttribute('data-effective-theme', 'light');

  const densitySaved = page.waitForResponse((response) => (
    response.url().endsWith('/api/preferences') && response.request().method() === 'PUT'
  ));
  await settings.getByLabel('List density').selectOption('compact');
  await densitySaved;
  await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');

  const resetSaved = page.waitForResponse((response) => (
    response.url().endsWith('/api/preferences') && response.request().method() === 'PUT'
  ));
  await settings.getByRole('button', { name: 'Reset layout' }).click();
  await resetSaved;
  await expect(page.locator('#settings-live')).toContainText('Vault and folder state were kept');
  await expect(page.locator('html')).toHaveAttribute('data-density', 'comfortable');
  await expectNoSeriousOrCriticalAxeViolations(page);

  await page.keyboard.press('Escape');
  await expect(page.locator('#settings-open')).toBeFocused();
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-density', 'comfortable');
});

test('Help explains the current capability boundary and copies redacted diagnostics', async ({ page, baseURL }) => {
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(baseURL);
  await expect(page.locator('#context-line')).toContainText('local / playwright');

  await page.locator('#help-open').click();
  const help = page.getByRole('dialog', { name: 'Help' });
  await expect(help.getByRole('heading', { name: 'Help' })).toBeVisible();
  await expect(help.getByRole('button', { name: 'Close Help' })).toBeFocused();
  expect(await help.locator('.utility-sheet-body').evaluate((body) => body.scrollTop)).toBe(0);
  await expect(help.getByRole('heading', { name: 'Effective context' })).toBeVisible();
  await expect(help.locator('#help-context-summary')).toContainText('local · playwright');
  await expect(help.locator('#help-capabilities')).toContainText(/Files: (Available|Unavailable)/);
  await expect(help.getByRole('heading', { name: 'Local-session security' })
    .locator('..')).toContainText('accepts connections only from this computer');
  await expect(help.getByRole('heading', { name: 'Local-session security' })
    .locator('..')).toContainText('Any app or browser on this computer with that link can access this session while Crosstache is running.');
  await expect(help.getByRole('heading', { name: 'Local-session security' })
    .locator('..')).toContainText('Do not share it.');
  await expect(help.locator('#help-config-path')).toContainText(/xv[\\/]xv\.conf$/);
  await expect(help.locator('#help-version')).not.toBeEmpty();

  await help.getByRole('button', { name: 'Copy redacted diagnostics' }).click();
  await expect(help.locator('#help-copy-status')).toHaveText('Diagnostics copied.');
  const diagnostics = await page.evaluate(() => navigator.clipboard.readText());
  expect(diagnostics).toContain('Crosstache');
  expect(diagnostics).toContain('Backend: local');
  expect(diagnostics).toContain('Vault: playwright');
  expect(diagnostics).toContain('Security policy limit (seconds): none');
  expect(diagnostics).toContain('Effective protected-value timeout (seconds): 30');
  expect(diagnostics).not.toContain('Protected value timeout:');
  expect(diagnostics).not.toMatch(/https?:\/\/|127\.0\.0\.1|localhost|token=/i);
  await expectNoSeriousOrCriticalAxeViolations(page);

  const closeHitTest = await help.locator('#help-close').evaluate((button) => {
    const rect = (element) => {
      const box = element.getBoundingClientRect();
      return {
        x: box.x,
        y: box.y,
        width: box.width,
        height: box.height,
        top: box.top,
        right: box.right,
        bottom: box.bottom,
        left: box.left,
      };
    };
    const style = (element) => {
      const computed = getComputedStyle(element);
      return {
        position: computed.position,
        zIndex: computed.zIndex,
        pointerEvents: computed.pointerEvents,
        overflow: computed.overflow,
        transform: computed.transform,
      };
    };
    const header = button.closest('.utility-sheet-header');
    const dialog = button.closest('#help-dialog');
    const body = dialog.querySelector('.utility-sheet-body');
    const box = button.getBoundingClientRect();
    const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
    return {
      button: { rect: rect(button), style: style(button) },
      header: { rect: rect(header), style: style(header) },
      body: { rect: rect(body), style: style(body) },
      dialog: { rect: rect(dialog), style: style(dialog) },
      scroll: { dialogTop: dialog.scrollTop, windowY: window.scrollY },
      hit: {
        tag: hit?.tagName ?? null,
        id: hit?.id ?? null,
        className: hit?.className ?? null,
        isClose: Boolean(hit?.closest?.('#help-close')),
      },
    };
  });
  expect(closeHitTest.hit.isClose, JSON.stringify(closeHitTest, null, 2)).toBe(true);
  await page.getByRole('button', { name: 'Close Help' }).click();
  await expect(page.locator('#help-open')).toBeFocused();
});

test('utility sheets become full-screen at 390px and disable motion when requested', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto(baseURL);
  await page.locator('#help-open').click();
  const sheet = page.getByRole('dialog', { name: 'Help' });
  await expect(sheet.getByRole('button', { name: 'Close Help' })).toBeFocused();
  expect(await sheet.locator('.utility-sheet-body').evaluate((body) => body.scrollTop)).toBe(0);
  const box = await sheet.boundingBox();
  expect(box?.x).toBe(0);
  expect(box?.width).toBe(390);
  const motion = await sheet.evaluate((element) => getComputedStyle(element).animationDuration);
  expect(Number.parseFloat(motion)).toBeLessThanOrEqual(0.00002);
  await expectNoSeriousOrCriticalAxeViolations(page);
  await page.screenshot({
    path: '/tmp/crosstache-task5-help-390.png',
    fullPage: false,
    animations: 'disabled',
  });
});
