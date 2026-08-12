import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { verifyRuntimeAssets } from './verify-assets.mjs';

describe('runtime asset verification', () => {
  it.each([
    ['changed URL', (manifest: any) => { manifest.downloads[0].url = 'https://example.invalid/llama.zip'; }],
    ['changed archive basename', (manifest: any) => { manifest.downloads[1].archive = 'other.tgz'; }],
    ['unsafe archive path', (manifest: any) => { manifest.downloads[2].archive = '../eng.traineddata'; }],
    ['missing license download', (manifest: any) => { manifest.downloads = manifest.downloads.filter((download: any) => download.id !== 'llama.cpp-license'); }],
    ['extra download', (manifest: any) => { manifest.downloads.push({ ...manifest.downloads[0], id: 'extra' }); }],
    ['changed vcpkg repository', (manifest: any) => { manifest.vcpkg.repository = 'https://example.invalid/vcpkg.git'; }],
  ])('rejects a %s in the exact runtime acquisition contract', async (_label, mutate) => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-pins-'));
    const source = JSON.parse(await readFile(join(process.cwd(), 'src-tauri/resources/runtime-assets.json'), 'utf8'));
    mutate(source);
    const manifestPath = join(root, 'runtime-assets.json');
    await writeFile(manifestPath, JSON.stringify(source));

    await expect(verifyRuntimeAssets(manifestPath, { root, requireExpectedPins: true }))
      .rejects.toThrow(/download|URL|archive|vcpkg/i);
  });

  it('does not treat an empty bundle inventory as a verified Windows runtime', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-empty-'));
    const manifestPath = join(root, 'runtime-assets.json');
    await writeFile(manifestPath, JSON.stringify({ schema_version: 1, downloads: [], bundled_files: [], license_files: [] }));

    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true }))
      .rejects.toThrow(/empty/i);
  });

  it('fails closed when a signed bundled asset is missing or tampered', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-'));
    const bytes = Buffer.from('trusted runtime fixture');
    const manifest = {
      schema_version: 1,
      downloads: [],
      bundled_files: [{
        path: 'src-tauri/binaries/runtime.exe',
        install_path: 'runtime.exe',
        packages: [{ name: 'test-runtime', version: '1.0.0' }],
        size: bytes.length,
        sha256: createHash('sha256').update(bytes).digest('hex'),
      }],
      license_files: [{
        path: 'licenses/runtime.txt', install_path: 'licenses/runtime.txt', size: 7,
        sha256: createHash('sha256').update('license').digest('hex'),
      }],
    };
    const manifestPath = join(root, 'runtime-assets.json');
    await mkdir(join(root, 'licenses'));
    await writeFile(join(root, 'licenses/runtime.txt'), 'license');
    await writeFile(manifestPath, JSON.stringify(manifest));

    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true }))
      .rejects.toThrow(/missing/i);
    await writeFile(join(root, 'runtime.exe'), 'tampered');
    manifest.bundled_files[0].path = 'runtime.exe';
    await writeFile(manifestPath, JSON.stringify(manifest));
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true }))
      .rejects.toThrow(/size|SHA-256/i);
    await writeFile(join(root, 'runtime.exe'), bytes);
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true }))
      .resolves.toMatchObject({ verifiedFiles: 1 });
  });

  it('rejects manifest paths that escape the verification root', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-path-'));
    const manifestPath = join(root, 'runtime-assets.json');
    await writeFile(manifestPath, JSON.stringify({
      schema_version: 1,
      downloads: [],
      bundled_files: [{ path: '../escape.exe', install_path: 'escape.exe', packages: [{ name: 'test-runtime', version: '1.0.0' }], size: 0, sha256: '0'.repeat(64) }],
      license_files: [{ path: 'notice.txt', install_path: 'licenses/notice.txt', size: 0, sha256: createHash('sha256').update('').digest('hex') }],
    }));

    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true }))
      .rejects.toThrow(/unsafe/i);
  });

  it('rejects unsafe or duplicate packaged paths and requires a license inventory', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-install-path-'));
    const bytes = Buffer.from('runtime');
    const file = { path: 'runtime.exe', install_path: '../runtime.exe', packages: [{ name: 'test-runtime', version: '1.0.0' }], size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
    await writeFile(join(root, 'runtime.exe'), bytes);
    await writeFile(join(root, 'notice.txt'), 'notice');
    const license = { path: 'notice.txt', install_path: 'licenses/notice.txt', size: 6, sha256: createHash('sha256').update('notice').digest('hex') };
    const manifestPath = join(root, 'runtime-assets.json');
    await writeFile(manifestPath, JSON.stringify({ schema_version: 1, downloads: [], bundled_files: [file] }));
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true })).rejects.toThrow(/license_files/i);

    await writeFile(manifestPath, JSON.stringify({ schema_version: 1, downloads: [], bundled_files: [file], license_files: [license] }));
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true })).rejects.toThrow(/packaged|install/i);

    file.install_path = 'runtime.exe';
    await writeFile(manifestPath, JSON.stringify({ schema_version: 1, downloads: [], bundled_files: [file, { ...file, path: 'copy.exe' }], license_files: [license] }));
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true })).rejects.toThrow(/duplicate packaged/i);
  });

  it('requires exact package owner and version metadata for each runtime file', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-assets-packages-'));
    await mkdir(join(root, 'licenses'));
    await writeFile(join(root, 'runtime.dll'), 'runtime');
    await writeFile(join(root, 'licenses/notice.txt'), 'notice');
    const runtime = { path: 'runtime.dll', install_path: 'runtime.dll', packages: [{ name: 'libpng', version: '' }], size: 7, sha256: createHash('sha256').update('runtime').digest('hex') };
    const license = { path: 'licenses/notice.txt', install_path: 'licenses/notice.txt', size: 6, sha256: createHash('sha256').update('notice').digest('hex') };
    const manifestPath = join(root, 'runtime-assets.json');
    await writeFile(manifestPath, JSON.stringify({ schema_version: 1, downloads: [], bundled_files: [runtime], license_files: [license] }));
    await expect(verifyRuntimeAssets(manifestPath, { root, requireBundled: true })).rejects.toThrow(/package version/i);
  });
});
