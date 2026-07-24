import {
  test,
  expect,
  expectNoSeriousOrCriticalAxeViolations,
} from './fixtures.js';

const fixedUpdatedOn = '2024-01-15T12:00:00Z';
const visualSecrets = [
  {
    name: 'stripe-production-primary-api-credential-with-rotation-history',
    folder: 'teams/platform/production/payments',
    groups: ['platform', 'payments'],
    note: 'Primary checkout integration; rotate after the quarterly release.',
  },
  {
    name: 'customer-identity-signing-key-for-europe-west',
    folder: 'teams/platform/production/identity',
    groups: ['platform', 'identity'],
    note: 'Regional signing material.',
  },
  {
    name: 'warehouse-analytics-read-only-service-account',
    folder: 'teams/data/production/analytics',
    groups: ['data'],
    note: 'Read-only reporting access.',
  },
  {
    name: 'incident-response-break-glass-credential',
    folder: 'teams/security/emergency',
    groups: ['security'],
    note: 'Emergency use only.',
  },
  {
    name: 'developer-sandbox-example-token',
    folder: 'teams/platform/sandbox',
    groups: ['platform'],
    note: 'Non-production fixture.',
  },
];

async function seedLongNames(page) {
  const result = await page.evaluate(async (secrets) => {
    const context = window.__xvTestStoreSnapshot().context;
    const scope = new URLSearchParams({
      alias: context.workspace.alias,
      backend: context.backend,
      vault: context.vault,
    });
    const token = sessionStorage.getItem('xv.ui.token');
    const failures = [];
    for (const secret of secrets) {
      const response = await fetch(`/api/secrets/${encodeURIComponent(secret.name)}?${scope}`, {
        method: 'PUT',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          value: 'visual-fixture-value',
          content_type: 'text/plain',
          folder: secret.folder,
          groups: secret.groups,
          note: secret.note,
        }),
      });
      if (!response.ok) failures.push(`${secret.name}: ${response.status}`);
    }
    return failures;
  }, visualSecrets);
  expect(result).toEqual([]);
}

async function stabilizeVisualSurface(page) {
  await page.route('**/api/context', async (route) => {
    const response = await route.fetch();
    const context = await response.json();
    await route.fulfill({
      response,
      json: {
        ...context,
        project: {
          name: 'visual-project',
          path: '/workspace/visual-project',
        },
        version: '0.0.0-visual',
      },
    });
  });
  await page.route('**/api/secrets?*', async (route) => {
    const response = await route.fetch();
    const secrets = await response.json();
    await route.fulfill({
      response,
      json: secrets.map((secret) => ({
        ...secret,
        updated_on: fixedUpdatedOn,
      })),
    });
  });
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const style = document.createElement('style');
      style.dataset.visualFixture = 'stable-font-and-caret';
      style.textContent = `
        :root, body, button, input, select, textarea {
          font-family: Arial, sans-serif !important;
        }
        * {
          caret-color: transparent !important;
        }
      `;
      document.head.appendChild(style);
    }, { once: true });
  });
}

async function expectNoHorizontalOverflow(page) {
  await expect.poll(() => page.evaluate(() => ({
    body: document.body.scrollWidth <= document.body.clientWidth,
    root: document.documentElement.scrollWidth <= document.documentElement.clientWidth,
  }))).toEqual({ body: true, root: true });
}

for (const theme of ['light', 'dark']) {
  test(`${theme} vault workspace`, async ({ page, baseURL }) => {
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    await stabilizeVisualSurface(page);
    await page.goto(baseURL);
    await expect(page.locator('#context-line')).toHaveText(
      'local / playwright · visual-project · browser',
    );
    await seedLongNames(page);
    await page.reload();

    await expect(page.getByRole('button', {
      name: `Edit secret ${visualSecrets[0].name}`,
    })).toBeVisible();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'system');
    await page.evaluate(() => document.fonts.ready);
    await expectNoHorizontalOverflow(page);
    await expectNoSeriousOrCriticalAxeViolations(page);
    await expect(page).toHaveScreenshot(`${theme}-vault-workspace.png`, {
      fullPage: true,
      animations: 'disabled',
    });
  });
}
