import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { access, copyFile, cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { Builder, By, until } from 'selenium-webdriver';

const exec = promisify(execFile);
const root = fileURLToPath(new URL('../../../', import.meta.url));
const windows = process.platform === 'win32';
const suffix = windows ? '.exe' : '';
const output = path.join(root, 'target/real-app-results');
const binaryDir = process.env.E2E_BINARY_DIR || path.join(root, 'target/debug');

async function waitFor(check, description, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await delay(100);
  }
  throw new Error('Timed out waiting for ' + description);
}

async function stopDriver(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  const stopped = once(child, 'exit');
  if (windows) {
    // Stop only this test's process tree, including the native driver/app.
    await exec('taskkill', ['/PID', String(child.pid), '/T', '/F']);
  } else {
    process.kill(-child.pid, 'SIGTERM');
  }
  await stopped;
}

// Compile an example using dev dependencies, so the desktop probe stays out of
// installed app/CLI binaries. Cargo reports its path even with a custom target dir.
async function buildProbe() {
  const built = await exec('cargo', [
    'build', '--locked', '-p', 'rotator-core', '--example', 'os-wallpaper-probe',
    '-j', '4', '--message-format=json',
  ], { cwd: root, timeout: 180000, maxBuffer: 8 * 1024 * 1024 });
  const artifacts = built.stdout.trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
  const binary = artifacts.find(item => item.reason === 'compiler-artifact' && item.target.name === 'os-wallpaper-probe')?.executable;
  assert.ok(binary, 'Cargo reported the OS probe executable');
  return binary;
}

function startProbe(binary) {
  let log = '';
  let launchError;
  const child = spawn(binary, [], { cwd: root, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  child.on('error', error => { launchError = error; });
  child.stdin.on('error', error => { launchError = error; });
  child.stderr.on('data', bytes => { log += bytes; });
  const exited = new Promise(resolve => child.once('close', code => resolve(code)));
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const response = async () => {
    const abort = new AbortController();
    try {
      const next = await Promise.race([
        lines.next(),
        delay(20000, null, { signal: abort.signal }).then(() => { throw new Error('OS probe timed out: ' + log); }),
      ]);
      if (launchError) throw launchError;
      assert.ok(!next.done, 'OS probe closed unexpectedly: ' + log);
      log += next.value + '\n';
      return JSON.parse(next.value);
    } finally { abort.abort(); }
  };
  return {
    ready: async () => assert.equal((await response()).ready, true, 'original wallpaper saved before app launch'),
    verify: async (photo, phase) => {
      assert.match(photo.id, /^fav-(red|green|blue)$/, 'expected a bundled synthetic photo');
      child.stdin.write(JSON.stringify({ command: 'verify', file: photo.preview_file, colour: photo.id.slice(4) }) + '\n');
      const result = await response();
      await writeFile(path.join(output, phase + '-os-wallpaper.json'), JSON.stringify(result, null, 2));
      assert.equal(result.ok, true, 'independent OS read-back: ' + result.error);
      console.log(phase + ' OS wallpaper: ' + result.evidence);
      return result.evidence;
    },
    restore: async () => {
      // EOF restores even when a verification failed. The tray must exit first.
      child.stdin.end();
      try {
        assert.equal((await response()).restored, true, 'original wallpaper restored');
        assert.equal(await exited, 0, 'OS probe exit: ' + log);
      } finally {
        await writeFile(path.join(output, 'os-wallpaper-probe.log'), log);
      }
    },
  };
}

test('real popover Refresh changes the photo through Tauri and the worker', { timeout: 240000 }, async () => {
  assert.ok(['win32', 'linux'].includes(process.platform), 'tauri-driver supports Windows and Linux, not macOS');
  const app = path.join(binaryDir, 'wallpaper-rotator' + suffix);
  const cli = path.join(binaryDir, 'wallpaper-rotator-cli' + suffix);
  await access(app);
  await access(cli);
  await mkdir(output, { recursive: true });
  const home = await mkdtemp(path.join(tmpdir(), 'wallpaper-rotator-e2e-'));
  await cp(new URL('./fixtures/', import.meta.url), path.join(home, 'favorites'), { recursive: true });
  await writeFile(path.join(home, 'config.toml'), [
    'sources = ["favorites"]',
    'interval_minutes = 0',
    'paused = false',
    'start_at_login = false',
    'per_display = false',
    'cache_cap_mb = 20',
    '',
  ].join('\n'));
  // The driver, native WebDriver and real app inherit this same isolated root.
  const env = { ...process.env, WALLPAPER_ROTATOR_HOME: home, UNSPLASH_ACCESS_KEY: '', PEXELS_API_KEY: '' };
  const driverArgs = ['--port', '4444', '--native-port', '4445'];
  if (process.env.NATIVE_WEBDRIVER) driverArgs.push('--native-driver', process.env.NATIVE_WEBDRIVER);
  let driverLog = '';
  let launchError;
  let browser;
  const probe = startProbe(await buildProbe());
  let driver;
  const status = async () => (await exec(cli, ['status'], { env, timeout: 10000 })).stdout;
  const snapshot = () => browser.executeAsyncScript(function () {
    const done = arguments[arguments.length - 1];
    window.__TAURI__.core.invoke('get_state').then(done, error => done({ ipcError: String(error) }));
  });
  try {
    await probe.ready();
    driver = spawn(process.env.TAURI_DRIVER || 'tauri-driver', driverArgs, {
      cwd: root, env, windowsHide: true, detached: !windows, stdio: ['ignore', 'pipe', 'pipe'],
    });
    driver.on('error', error => { launchError = error; });
    driver.stdout.on('data', bytes => { driverLog += bytes; });
    driver.stderr.on('data', bytes => { driverLog += bytes; });
    await waitFor(async () => {
      if (launchError) throw launchError;
      assert.equal(driver.exitCode, null, 'tauri-driver exited: ' + driverLog);
      try {
        const response = await fetch('http://127.0.0.1:4444/status', { signal: AbortSignal.timeout(1000) });
        return response.ok && (await response.json()).value.ready;
      } catch { return false; }
    }, 'tauri-driver readiness');
    browser = await new Builder().disableEnvironmentOverrides().usingServer('http://127.0.0.1:4444').withCapabilities({
      browserName: 'wry',
      'tauri:options': { application: app, args: ['--show-popover'] },
    }).build();
    await browser.manage().setTimeouts({ script: 10000, implicit: 0, pageLoad: 30000 });
    const refresh = await browser.wait(until.elementLocated(By.id('btn-next')), 30000);
    await browser.wait(until.elementIsVisible(refresh), 30000);
    const before = await waitFor(async () => {
      const state = await snapshot();
      assert.ok(!state.ipcError, state.ipcError);
      assert.equal(state.error, null, 'initial app error');
      return !state.busy && state.current.length > 0 && state;
    }, 'initial favorite photo');
    await browser.wait(until.elementIsEnabled(refresh), 30000);
    await waitFor(() => browser.executeScript('return document.getElementById("preview").naturalWidth > 0'), 'real asset preview');
    const beforeStatus = await status();
    assert.match(beforeStatus, /^current: (?!none)/m);
    await writeFile(path.join(output, 'before-status.txt'), beforeStatus);
    const beforeOS = await probe.verify(before.current[0], 'before');
    // This is a native WebDriver click on the real button. No IPC is mocked.
    await refresh.click();
    const after = await waitFor(async () => {
      const state = await snapshot();
      assert.ok(!state.ipcError, state.ipcError);
      assert.equal(state.error, null, 'Refresh app error');
      return !state.busy && state.current[0]?.id !== before.current[0].id && state.current.length > 0 && state;
    }, 'Refresh to finish with a different photo');
    const afterOS = await probe.verify(after.current[0], 'after');
    assert.notEqual(afterOS, beforeOS, 'OS wallpaper changed after Refresh');
    const afterStatus = await status();
    assert.match(afterStatus, /^current: (?!none)/m);
    assert.notEqual(after.current[0].preview_file, before.current[0].preview_file, 'cached photo path changed');
    assert.notEqual(afterStatus.match(/^current:.*$/m)[0], beforeStatus.match(/^current:.*$/m)[0], 'CLI current photo changed');
    assert.ok(afterStatus.includes(after.current[0].id), 'CLI and live popover agree on the new photo');
    assert.ok(after.current[0].preview_file.startsWith(home + path.sep), 'app used the isolated home');
    await writeFile(path.join(output, 'after-status.txt'), afterStatus);
    const appLog = await readFile(path.join(home, 'rotator.log'), 'utf8');
    assert.doesNotMatch(appLog, /\berror\b|could not|failed/i, 'app log has no errors');
    await writeFile(path.join(output, 'snapshots.json'), JSON.stringify({ before, after }, null, 2));
    await writeFile(path.join(output, 'popover.png'), await browser.takeScreenshot(), 'base64');
    console.log('Refresh changed ' + before.current[0].id + ' -> ' + after.current[0].id);
    console.log('Independent OS read-back verified before and after Refresh.');
    console.log('Isolated home retained at ' + home);
  } finally {
    try {
      // Retain state/logs on failures too. Leave the cached wallpaper on disk.
      await writeFile(path.join(output, 'home.txt'), home);
      for (const file of ['config.toml', 'state.json', 'rotator.log']) {
        await copyFile(path.join(home, file), path.join(output, file)).catch(error => {
          if (error.code !== 'ENOENT') throw error;
        });
      }
    } finally {
      try {
        if (browser) {
          // Exit the tray explicitly; closing its window only hides it. The
          // session may disappear as quit runs, so teardown also stops the tree.
          await browser.executeScript('window.__TAURI__.core.invoke("quit");').catch(() => {});
          await browser.quit().catch(() => {});
        }
      } finally {
        try {
          await stopDriver(driver);
          await writeFile(path.join(output, 'driver.log'), driverLog);
        } finally {
          await probe.restore();
        }
      }
    }
  }
});
