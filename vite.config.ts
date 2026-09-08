import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 1420,
    strictPort: true,
  },
  test: {
    environment: 'jsdom',
    setupFiles: './vitest.setup.ts',
    exclude: ['tests/e2e/**', 'node_modules/**', 'dist/**'],
    // Several script tests shell out to `cargo metadata`, `git`, and
    // PowerShell to assert against the real repository rather than a
    // fixture. That is the point of them, but it means their runtime is a
    // cold process launch on whatever machine CI got, and vitest's 5s
    // default has failed release-blocking runs on timing alone - never on
    // an assertion. The work is bounded; the clock was the wrong one.
    testTimeout: 60_000,
    hookTimeout: 60_000,
  },
});
