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

async function createLongSecret(page) {
  await page.locator('#new-secret').click();
  const form = page.locator('#secret-form');
  await form.locator('input[name="name"]').fill(longName);
  await form.locator('textarea[name="value"]').fill('responsive value');
  await form.locator('input[name="folder"]').fill(nestedFolder);
  await page.getByRole('button', { name: 'Create secret' }).click();
}

async function expectNoHorizontalOverflow(page) {
  await expect.poll(() => page.evaluate(() => ({
    body: document.body.scrollWidth <= document.body.clientWidth,
    root: document.documentElement.scrollWidth <= document.documentElement.clientWidth,
  }))).toEqual({ body: true, root: true });
}

for (const viewport of [
  { width: 768, height: 700 },
  { width: 390, height: 844 },
]) {
  test(`stacked rows preserve full identifiers without overflow at ${viewport.width}px`, async ({ page, baseURL }) => {
    await page.setViewportSize(viewport);
    await page.goto(baseURL);
    await createLongSecret(page);

    const list = page.locator('#secrets-stacked');
    await expect(list).toBeVisible();
    await expect(page.locator('#secrets-table')).toBeHidden();
    const activation = page.getByRole('button', { name: `Edit secret ${longName}` });
    await expect(activation).toBeVisible();
    await expect(activation).toContainText(longName);
    await expect(activation).toContainText(nestedFolder);
    await expect(page.locator('#secrets-stacked .stacked-identifier')).toHaveText(longName);
    await expectNoHorizontalOverflow(page);
    await expectNoSeriousOrCriticalAxeViolations(page);

    await page.getByRole('button', { name: 'Select', exact: true }).click();
    await expect(page.getByRole('checkbox', { name: `Select secret ${longName}` })).toBeVisible();
    await expect(page.getByRole('button', { name: `Select secret ${longName}` })).toHaveCount(1);
    await expectNoHorizontalOverflow(page);
  });
}

test('stacked mode preserves the empty-state action', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(baseURL);
  await expect(page.locator('#secrets-stacked')).toBeVisible();
  await expect(page.locator('#secrets-stacked').getByText('No secrets yet')).toBeVisible();
  await expect(page.locator('#secrets-stacked').getByRole('button', { name: 'New secret' })).toBeVisible();
});

test('desktop table keeps sorting and resizing controls above the breakpoint', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 769, height: 700 });
  await page.goto(baseURL);
  await expect(page.locator('#secrets-table')).toBeVisible();
  await expect(page.locator('#secrets-stacked')).toBeHidden();
  await expect(page.locator('#secrets-table [role="separator"]')).toHaveCount(4);
  await expect(page.locator('#secrets-table .sort-button')).toHaveCount(5);
});

test('sheets fill the viewport below 544px', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(baseURL);
  await page.locator('#new-secret').click();
  const box = await page.getByRole('dialog', { name: 'New secret' }).boundingBox();
  expect(box).toEqual(expect.objectContaining({ x: 0, y: 0, width: 390, height: 844 }));
});

test('desktop permits exercising the approved responsive breakpoints', async () => {
  const config = JSON.parse(await readFile(
    path.join(workspace, 'desktop/src-tauri/tauri.conf.json'),
    'utf8',
  ));
  expect(config.app.windows[0].minWidth).toBeLessThanOrEqual(390);
});
