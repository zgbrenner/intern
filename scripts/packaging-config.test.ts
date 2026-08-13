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

// Tauri refuses to build when a plugin's Rust crate and npm package differ in
// major/minor. A caret range on the crate let it float to 2.10.1 against an
// exactly-pinned npm 2.9.0, and the release died at the signing build with
// "Found version mismatched Tauri packages" - after twenty minutes of building
// Tesseract. Both sides are pinned exactly now, and this keeps them together.
it('keeps every Tauri plugin on the same version in Cargo.toml and package.json', async () => {
  const [cargo, packageJson] = await Promise.all([
    readFile('src-tauri/Cargo.toml', 'utf8'),
    readFile('package.json', 'utf8').then(JSON.parse),
  ]);

  const npmPlugins = Object.entries(packageJson.dependencies as Record<string, string>)
    .filter(([name]) => name.startsWith('@tauri-apps/plugin-'))
    .map(([name, version]) => ({ plugin: name.slice('@tauri-apps/plugin-'.length), version }));
  expect(npmPlugins.length).toBeGreaterThan(0);

  for (const { plugin, version } of npmPlugins) {
    const crate = new RegExp(`^tauri-plugin-${plugin}\\s*=\\s*"=?([^"]+)"`, 'm').exec(cargo);
    expect(crate, `tauri-plugin-${plugin} is missing from src-tauri/Cargo.toml`).not.toBeNull();
    // Exact pins on both sides: a range is what allowed the drift.
    expect(version, `@tauri-apps/plugin-${plugin} must be pinned exactly`).toMatch(/^\d+\.\d+\.\d+$/);
    expect(cargo).toContain(`tauri-plugin-${plugin} = "=${version}"`);
    expect(crate![1]).toBe(version);
  }
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
