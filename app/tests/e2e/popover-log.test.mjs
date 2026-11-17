import assert from 'node:assert/strict';
import { test } from 'node:test';
import { popoverEvents, popoverHidden } from './popover-log.mjs';

// The log of the red CI run on #98: the popover hid as it loaded.
const hiddenLog = [
  '1794912167 displays: Display 1: 1024x768 pixels, scale 1',
  '1794912173 popover webview created',
  '1794912173 popover hidden on blur; focus went to window class "Tauri Window" of wallpaper-rotator.exe (pid 3688, this app)',
  '1794912173 popover webview loaded',
  '1794912174 wallpaper set: fav-red.png by red (Favorites)',
  '1794912176 popover webview destroyed',
  '',
].join('\n');

const keptLog = [
  '1794913083 popover webview created',
  '1794913083 popover kept after a blur: it is still the foreground window (window class "Tauri Window")',
  '1794913083 popover webview loaded',
  '1794913084 wallpaper set: fav-blue.png by blue (Favorites)',
  '',
].join('\n');

test('popover events drop timestamps and other lines', () => {
  assert.deepEqual(popoverEvents(keptLog), [
    'popover webview created',
    'popover kept after a blur: it is still the foreground window (window class "Tauri Window")',
    'popover webview loaded',
  ]);
});

test('a hidden popover is named with its log lines', () => {
  const error = popoverHidden(hiddenLog, 'before Refresh');
  assert.match(error.message, /^popover hidden before Refresh:\npopover webview created\npopover hidden on blur; focus went to/);
  assert.match(error.message, /popover webview destroyed$/);
});

test('a dead session or null snapshot keeps the original failure', () => {
  const dead = new Error('invalid session id: session deleted as the browser has closed the connection');
  const error = popoverHidden(hiddenLog, 'while its state was read', dead);
  assert.equal(error.cause, dead);
  assert.match(error.message, /^popover hidden while its state was read:/);
  assert.match(error.message, /\(then: invalid session id: session deleted/);
});

test('a popover kept after a blur, or never hidden, is not an error', () => {
  assert.equal(popoverHidden(keptLog, 'before Refresh'), null);
  assert.equal(popoverHidden('', 'before Refresh'), null);
});
