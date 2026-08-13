import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  // The first navigation waits for Vite to pre-bundle dependencies, which on a
  // cold cache takes longer than the default 30s navigation budget on a busy
  // machine. Everything after it is fast; this only stops the first test from
  // failing for a reason that has nothing to do with the application.
  //
  // 60s was still not enough. `webServer.url` only proves the port answers;
  // Vite then optimizes dependencies on the first request, so the server is
  // "up" while `page.goto` blocks. Observed failure, on the first suite run
  // after a source change:
  //
  //   TimeoutError: page.goto: Timeout 60000ms exceeded.
  //     navigating to "http://127.0.0.1:1420/", waiting until "load"
  //
  // Repeat runs took ~50s for the whole suite, so this is cold-start cost, not
  // a slow page. Raised rather than retried: a retry hides it in CI and cannot
  // help a local run, and this gate also guards the release.
  timeout: 180_000,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'line',
  use: {
    baseURL: 'http://127.0.0.1:1420',
    navigationTimeout: 120_000,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1',
    url: 'http://127.0.0.1:1420',
    // Always start a dedicated server, the way CI does. Reusing one locally
    // meant a run could attach to a previous suite's server while it was still
    // shutting down: Playwright's readiness probe saw the port answer, then the
    // first `page.goto` hung until the navigation timeout. It failed five times
    // that way, always on the first test, always passing on retry, and raising
    // the timeout from 60s to 120s did not help - because the server was not
    // slow, it was going away. If the port is genuinely occupied this now fails
    // immediately and says so, instead of hanging for two minutes.
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
