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
});
