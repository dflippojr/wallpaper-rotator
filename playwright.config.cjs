const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './ui/tests',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: Boolean(process.env.CI),
  outputDir: 'target/popover-results',
  reporter: [['list'], ['html', { outputFolder: 'target/popover-report', open: 'never' }]],
  use: { viewport: { width: 340, height: 452 }, screenshot: 'only-on-failure', trace: 'retain-on-failure' },
  projects: ['chromium', 'webkit'].flatMap(browserName =>
    ['light', 'dark'].map(colorScheme => ({
      name: browserName + '-' + colorScheme,
      use: { browserName, colorScheme },
    }))),
});
