import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';

it('packages the license directory as a tree so vcpkg subpaths match the signed install manifest', async () => {
  const config = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8'));
  expect(config.bundle.windows.nsis.installMode).toBe('currentUser');
  expect(config.bundle.resources['resources/licenses/']).toBe('licenses/');
  expect(config.bundle.resources['resources/licenses/**/*']).toBeUndefined();

  const smoke = await readFile('scripts/smoke-installer.ps1', 'utf8');
  expect(smoke).toContain('$Relative = [string]$Entry.install_path');
  expect(smoke).toContain('Join-Path $InstallDirectory $Relative');
  expect(smoke).toContain('Start-Process -FilePath $App');
  expect(smoke).toContain('CloseMainWindow()');
  expect(smoke).toContain('WaitForExit(');
  expect(smoke).toContain('$EvidencePath');
});

it('leaves packaged-path collision detection to the checker that can actually see the property', async () => {
  const fetch = await readFile('scripts/fetch-windows-assets.ps1', 'utf8');
  // The staged file records are ordered hashtables, and PowerShell cannot
  // resolve a hashtable key as a named property when grouping. Grouping them by
  // the packaged-path key therefore put every file in one unnamed group and
  // reported a collision for any package holding more than one file. That check
  // never passed, and it is redundant: verify-assets.mjs performs it with real
  // property access and is covered by verify-assets.test.ts.
  expect(fetch).not.toMatch(/Group-Object\s+install_path/);
  expect(fetch).toMatch(/node .*verify-assets\.mjs.* --require-bundled/);
  // The manifest must be written before it is verified, or the check reads stale
  // contents. Match the invocation, not the comment above it.
  expect(fetch.indexOf('$Manifest.bundled_files = $BundledFiles'))
    .toBeLessThan(fetch.lastIndexOf('verify-assets.mjs'));
});
