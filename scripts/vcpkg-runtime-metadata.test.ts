import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { readVcpkgRuntimeMetadata } from './vcpkg-runtime-metadata.mjs';

it('derives DLL owners and exact versions from vcpkg status and installed lists', async () => {
  const root = await mkdtemp(join(tmpdir(), 'intern-vcpkg-metadata-'));
  await mkdir(join(root, 'vcpkg/info'), { recursive: true });
  await writeFile(join(root, 'vcpkg/status'), [
    'Package: tesseract\nVersion: 5.5.2\nArchitecture: x64-windows\nStatus: install ok installed',
    'Package: leptonica\nVersion: 1.85.0#1\nArchitecture: x64-windows\nStatus: install ok installed',
    'Package: libpng\nVersion: 1.6.50\nArchitecture: x64-windows\nStatus: install ok installed',
  ].join('\n\n'));
  await writeFile(join(root, 'vcpkg/info/tesseract_5.5.2_x64-windows.list'), 'x64-windows/tools/tesseract/tesseract.exe\n');
  await writeFile(join(root, 'vcpkg/info/leptonica_1.85.0#1_x64-windows.list'), 'x64-windows/bin/leptonica-6.dll\n');
  await writeFile(join(root, 'vcpkg/info/libpng_1.6.50_x64-windows.list'), 'x64-windows/bin/libpng16.dll\n');

  const metadata = await readVcpkgRuntimeMetadata(root, 'x64-windows');
  expect(metadata.owners['x64-windows/bin/leptonica-6.dll']).toEqual({ name: 'leptonica', version: '1.85.0#1' });
  expect(metadata.owners['x64-windows/bin/libpng16.dll']).toEqual({ name: 'libpng', version: '1.6.50' });
  expect(metadata.owners['x64-windows/tools/tesseract/tesseract.exe']).toEqual({ name: 'tesseract', version: '5.5.2' });
});
