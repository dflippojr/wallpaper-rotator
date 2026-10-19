const { test: base, expect } = require('@playwright/test');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const snapshot = require('./fixtures/state.json');

// Install before app.js executes. Every IPC call is recorded, including reads.
const test = base.extend({
  page: async ({ page }, use) => {
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await page.route(/^https?:/, route => route.abort());
    await page.addInitScript(({ snapshot, preview }) => {
      window.recordedCalls = [];
      const listeners = {};
      window.emitState = state => listeners.state?.({ payload: state });
      window.__TAURI__ = {
        core: {
          convertFileSrc: () => preview,
          invoke: async (command, args) => {
            window.recordedCalls.push({ command, args });
            if (command === 'get_state') return structuredClone(snapshot);
            if (command === 'save_settings') snapshot.settings = args.settings;
          },
        },
        event: { listen: async (name, callback) => { listeners[name] = callback; return () => {}; } },
      };
    }, {
      snapshot,
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

async function expectFits(page) {
  const problems = await page.evaluate(() => {
    const issues = [];
    const visible = el => el.getClientRects().length > 0;
    for (const el of document.querySelectorAll('html, body, form, form section')) {
      if (visible(el) && (el.scrollHeight > el.clientHeight + 1 || el.scrollWidth > el.clientWidth + 1)) {
        issues.push('overflow: ' + (el.id || el.tagName));
      }
    }
    const controls = [...document.querySelectorAll('button, input, select')].filter(visible);
    for (const el of controls) {
      const r = el.getBoundingClientRect();
      if (r.left < 0 || r.top < 0 || r.right > 340 || r.bottom > 452) issues.push('outside viewport: ' + el.outerHTML);
      const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
      if (hit !== el && !el.contains(hit)) issues.push('covered control: ' + el.outerHTML);
    }
    const section = [...document.querySelectorAll('form section')].find(visible);
    const blocks = [document.querySelector('.settings-tabs'), ...section.children,
      document.querySelector('.actions'), document.querySelector('#btn-quit')].filter(visible);
    for (let i = 0; i < blocks.length; i++) {
      const a = blocks[i].getBoundingClientRect();
      for (const el of blocks.slice(i + 1)) {
        const b = el.getBoundingClientRect();
        if (Math.min(a.right, b.right) - Math.max(a.left, b.left) > 1 &&
            Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 1) {
          issues.push('overlap: ' + blocks[i].outerHTML + ' / ' + el.outerHTML);
        }
      }
    }
    return issues;
  });
  expect(problems).toEqual([]);
}

test('every settings view fits 340 by 452 without clipping, overlap or scrolling', async ({ page }) => {
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  for (const name of ['Sources', 'More sources', 'API keys', 'Rotation']) {
    await page.getByRole('button', { name, exact: true }).click();
    await expectFits(page);
  }
});

test('Refresh sends the shown display index, including after a state event', async ({ page }) => {
  await page.getByRole('button', { name: 'New photo for Built-in', exact: true }).click();
  await page.getByRole('button', { name: 'External', exact: true }).click();
  await page.evaluate(state => window.emitState(state), snapshot);
  await page.getByRole('button', { name: 'New photo for External', exact: true }).click();
  expect(await calls(page, 'next_wallpaper')).toEqual([
    { command: 'next_wallpaper', args: { index: 0 } },
    { command: 'next_wallpaper', args: { index: 1 } },
  ]);
});

test('Refresh requests all displays when one photo covers them', async ({ page }) => {
  await page.evaluate(state => window.emitState({ ...state, current: [state.current[0]] }), snapshot);
  await expect(page.locator('#displays')).toBeHidden();
  await page.getByRole('button', { name: 'Next wallpaper', exact: true }).click();
  expect(await calls(page, 'next_wallpaper')).toEqual([{ command: 'next_wallpaper', args: { index: null } }]);
});

test('saving settings sends the complete edited payload and keys', async ({ page }) => {
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.locator('[data-source="daily"]').uncheck();
  await page.locator('[data-source="wallhaven"]').uncheck();
  await page.locator('#f-query').fill('ocean');
  await page.locator('#f-topics').fill('nature');
  await page.getByRole('button', { name: 'More sources', exact: true }).click();
  await page.locator('#f-collections').fill('67890');
  await page.locator('#f-username').fill('test-artist');
  await page.getByRole('button', { name: 'API keys', exact: true }).click();
  await page.locator('#f-key').fill('synthetic-unsplash-key');
  await page.locator('#f-pexels').fill('synthetic-pexels-key');
  await page.getByRole('button', { name: 'Rotation', exact: true }).click();
  await page.locator('#f-interval').selectOption('360');
  await page.locator('#f-cap').fill('80');
  await page.locator('#f-each').uncheck();
  await page.locator('#f-paused').check();
  await page.locator('#f-login').check();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  expect(await calls(page, 'save_settings')).toEqual([{
    command: 'save_settings',
    args: {
      settings: { sources: ['unsplash', 'pexels', 'favorites'], query: 'ocean', topics: 'nature',
        collections: '67890', username: 'test-artist', interval_minutes: 360, paused: true,
        cache_cap_mb: 80, start_at_login: true, per_display: false },
      accessKey: 'synthetic-unsplash-key', pexelsKey: 'synthetic-pexels-key',
    },
  }]);
  await expect(page.locator('#view-settings')).toBeHidden();
});

test('blank keys preserve saved credentials and no source prevents saving', async ({ page }) => {
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  expect((await calls(page, 'save_settings'))[0].args).toEqual({
    settings: snapshot.settings, accessKey: null, pexelsKey: null,
  });
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  for (const source of snapshot.settings.sources) await page.locator('[data-source="' + source + '"]').uncheck();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('alert')).toHaveText('Tick at least one photo source.');
  expect(await calls(page, 'save_settings')).toHaveLength(1);
});
