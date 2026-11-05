// Issue #81: "Open favorites folder" next to Save to Favorites and in
// Settings next to My favorites. The backend reads the folder from config,
// so the page must call open_favorites with no path.
const { test: base, expect } = require('@playwright/test');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const snapshot = require('./fixtures/state.json');

// A recorded fake Tauri object, as in popover.spec.cjs. `openError` makes
// open_favorites reject the way the Rust command does when it can't open.
const test = base.extend({
  openError: [null, { option: true }],
  page: async ({ page, openError }, use) => {
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await page.route(/^https?:/, route => route.abort());
    await page.addInitScript(({ snapshot, preview, openError }) => {
      window.recordedCalls = [];
      window.__TAURI__ = {
        core: {
          convertFileSrc: () => preview,
          invoke: async (command, args) => {
            window.recordedCalls.push({ command, args });
            if (command === 'get_state') return structuredClone(snapshot);
            if (command === 'open_favorites' && openError) throw openError;
          },
        },
        event: { listen: async () => () => {} },
      };
    }, {
      snapshot,
      openError,
      preview: pathToFileURL(path.resolve('app/tests/e2e/fixtures/blue.png')).href,
    });
    await page.goto(pathToFileURL(path.resolve('ui/index.html')).href);
    await expect(page.locator('#credit')).toHaveText('By Fixture artist');
    await use(page);
    expect(errors, 'browser console and uncaught errors').toEqual([]);
  },
});

async function calls(page, command) {
  return page.evaluate(command => window.recordedCalls.filter(call => call.command === command), command);
}

// Fully inside the 340 by 452 popover and not covered by anything.
async function expectUncovered(locator) {
  const box = await locator.boundingBox();
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(340);
  expect(box.y + box.height).toBeLessThanOrEqual(452);
  const hit = await locator.evaluate(el => {
    const r = el.getBoundingClientRect();
    const top = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return top === el || el.contains(top);
  });
  expect(hit).toBe(true);
}

test('the popover link sits next to Save to Favorites and opens the configured folder', async ({ page }) => {
  const save = page.getByRole('button', { name: 'Save to Favorites', exact: true });
  const open = page.getByRole('button', { name: 'Open favorites folder', exact: true });
  await expect(open).toBeVisible();
  await expectUncovered(open);
  await expectUncovered(save);
  const [s, o] = [await save.boundingBox(), await open.boundingBox()];
  expect(Math.abs(s.y - o.y), 'same row').toBeLessThan(2);
  expect(o.x).toBeGreaterThan(s.x + s.width);
  await expect(open).toHaveAttribute('title', 'Open ' + snapshot.favorites_dir);

  await open.click();
  // No path: the backend reads the folder from config.
  expect(await calls(page, 'open_favorites')).toEqual([{ command: 'open_favorites', args: undefined }]);
  expect(await calls(page, 'favorite')).toEqual([]);
});

test('the Settings link sits next to My favorites and opens the configured folder', async ({ page }) => {
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  const open = page.getByRole('button', { name: 'Open favorites folder', exact: true });
  await expect(open).toBeVisible();
  await expectUncovered(open);
  const mine = page.locator('label.check', { has: page.locator('[data-source="favorites"]') });
  const [m, o] = [await mine.boundingBox(), await open.boundingBox()];
  expect(Math.abs((m.y + m.height / 2) - (o.y + o.height / 2)), 'same row').toBeLessThan(4);
  expect(o.x).toBeGreaterThan(m.x);

  await open.click();
  expect(await calls(page, 'open_favorites')).toEqual([{ command: 'open_favorites', args: undefined }]);
  // Opening the folder neither saves nor leaves Settings.
  expect(await calls(page, 'save_settings')).toEqual([]);
  await expect(page.locator('#view-settings')).toBeVisible();
});

test.describe('when the folder cannot be opened', () => {
  const reason = 'could not create /synthetic/Favorites: Permission denied (os error 13)';
  test.use({ openError: reason });

  test('the popover says why', async ({ page }) => {
    await page.getByRole('button', { name: 'Open favorites folder', exact: true }).click();
    await expect(page.getByRole('status')).toHaveText(reason);
    await expect(page.getByRole('status')).toHaveClass(/error/);
  });

  test('Settings says why', async ({ page }) => {
    await page.getByRole('button', { name: 'Settings', exact: true }).click();
    await page.getByRole('button', { name: 'Open favorites folder', exact: true }).click();
    await expect(page.getByRole('alert')).toHaveText(reason);
  });
});
