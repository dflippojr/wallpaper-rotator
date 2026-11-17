// Reading the app log's popover lines for the real-app journey (#98). The
// popover hides on blur and its webview is destroyed 3 s later; the journey
// names that instead of failing on a dead WebDriver session or a null snapshot.

/** The log's popover lines, without their timestamps. */
export function popoverEvents(log) {
  return log.split('\n').map(line => line.replace(/^\d+ /, '')).filter(line => line.startsWith('popover '));
}

/**
 * An error saying the popover hid `when`, with its log lines and the original
 * failure if there was one; null when the log records no hide or destroy.
 */
export function popoverHidden(log, when, cause) {
  const events = popoverEvents(log);
  if (!events.some(event => /^popover (hidden|webview destroyed)/.test(event))) return null;
  const message = 'popover hidden ' + when + ':\n' + events.join('\n') + (cause ? '\n(then: ' + cause.message + ')' : '');
  return new Error(message, cause ? { cause } : undefined);
}
